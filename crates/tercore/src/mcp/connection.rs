//! A live MCP session: handshake, request/response demultiplexing, tool calls.
//!
//! One background task owns the reader half of the transport and routes each
//! JSON-RPC message: responses to the waiting caller by id, server→client
//! requests get a `-32601` reply (this client does not implement sampling), and
//! notifications are forwarded to subscribers.

use super::protocol::*;
use super::transport::{MessageReader, MessageWriter, SpawnedTransport};
use crate::error::{Result, TerError};
use crate::logging::{ActivityLog, LogSource};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio::task::JoinHandle;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(45);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Tool calls can legitimately be slow (mesh/material generation, playtests).
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(300);

pub struct Connection {
    writer: Arc<dyn MessageWriter>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    reader_task: Mutex<Option<JoinHandle<()>>>,
    closed: Arc<AtomicBool>,
    next_id: AtomicU64,
    child: Mutex<Option<tokio::process::Child>>,
    log: ActivityLog,
    notifications: broadcast::Sender<Value>,
    pub init: InitializeResult,
    pub tools: Vec<McpTool>,
    pub description: String,
}

impl Connection {
    /// Performs the MCP handshake and discovers tools.
    pub async fn open(spawned: SpawnedTransport, log: ActivityLog) -> Result<Self> {
        let SpawnedTransport {
            writer,
            reader,
            child,
            description,
        } = spawned;
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (notification_tx, _) = broadcast::channel(128);

        let reader_task = tokio::spawn(reader_loop(
            reader,
            pending.clone(),
            writer.clone(),
            closed.clone(),
            log.clone(),
            notification_tx.clone(),
        ));

        let mut connection = Self {
            writer,
            pending,
            reader_task: Mutex::new(Some(reader_task)),
            closed,
            next_id: AtomicU64::new(1),
            child: Mutex::new(child),
            log: log.clone(),
            notifications: notification_tx,
            init: InitializeResult::default(),
            tools: Vec::new(),
            description,
        };

        let init_value = connection
            .request_with_timeout("initialize", initialize_params(), HANDSHAKE_TIMEOUT)
            .await?;
        let init: InitializeResult = serde_json::from_value(init_value).map_err(|e| {
            TerError::Mcp(format!(
                "MCP server returned an invalid initialize result: {e}"
            ))
        })?;

        if init.protocol_version.is_empty() {
            return Err(TerError::Mcp(
                "MCP server did not report a protocol version".into(),
            ));
        }
        if !SUPPORTED_VERSIONS.contains(&init.protocol_version.as_str()) {
            log.warn(
                LogSource::Mcp,
                format!(
                    "MCP server negotiated protocol {}; this client is tested with {}",
                    init.protocol_version, PROTOCOL_VERSION
                ),
            );
        }
        // Keep the negotiated session details: the status bar and diagnostics
        // report the server name and protocol version from here.
        connection.init = init.clone();
        connection
            .writer
            .set_protocol_version(&init.protocol_version)
            .await;

        // Spec: the client must send this notification before other requests.
        connection
            .notify("notifications/initialized", None)
            .await?;

        log.info(
            LogSource::Mcp,
            format!(
                "connected to {} v{} (protocol {})",
                if init.server_info.name.is_empty() {
                    "MCP server"
                } else {
                    &init.server_info.name
                },
                init.server_info.version,
                init.protocol_version
            ),
        );

        connection.tools = connection.fetch_all_tools().await?;
        log.info(
            LogSource::Mcp,
            format!("discovered {} tools", connection.tools.len()),
        );

        Ok(connection)
    }

    async fn fetch_all_tools(&self) -> Result<Vec<McpTool>> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        // Bound pagination so a misbehaving server cannot loop forever.
        for _ in 0..50 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let value = self
                .request_with_timeout("tools/list", params, DEFAULT_REQUEST_TIMEOUT)
                .await?;
            let page: ToolsListResult = serde_json::from_value(value).map_err(|e| {
                TerError::Mcp(format!("MCP server returned an invalid tools/list result: {e}"))
            })?;
            all.extend(page.tools);
            match page.next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        all.sort_by(|a, b| a.name.cmp(&b.name));
        all.dedup_by(|a, b| a.name == b.name);
        Ok(all)
    }

    pub async fn refresh_tools(&mut self) -> Result<Vec<McpTool>> {
        self.tools = self.fetch_all_tools().await?;
        Ok(self.tools.clone())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(TerError::Mcp(
                "MCP connection is closed; reconnect to continue".into(),
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let message = serde_json::to_value(JsonRpcRequest::new(id, method, params))
            .map_err(|e| TerError::json("MCP request", e))?;
        if let Err(e) = self.writer.send(&message).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(TerError::Mcp(format!(
                    "MCP request `{method}` timed out after {}s",
                    timeout.as_secs()
                )))
            }
            Ok(Err(_)) => {
                self.pending.lock().await.remove(&id);
                Err(TerError::Mcp(format!(
                    "MCP connection dropped while waiting for `{method}`"
                )))
            }
            Ok(Ok(response)) => {
                if let Some(error) = response.error {
                    return Err(TerError::Mcp(format!(
                        "MCP error {} from `{method}`: {}{}",
                        error.code,
                        error.message,
                        error
                            .data
                            .as_ref()
                            .map(|d| format!(" ({})", truncate(&d.to_string(), 300)))
                            .unwrap_or_default()
                    )));
                }
                Ok(response.result.unwrap_or(Value::Null))
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let message = serde_json::to_value(JsonRpcNotification::new(method, params))
            .map_err(|e| TerError::json("MCP notification", e))?;
        self.writer.send(&message).await
    }

    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult> {
        let value = self
            .request_with_timeout(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
                TOOL_CALL_TIMEOUT,
            )
            .await?;
        serde_json::from_value(value).map_err(|e| {
            TerError::Mcp(format!(
                "MCP server returned an invalid tools/call result: {e}"
            ))
        })
    }

    pub async fn ping(&self) -> Result<()> {
        self.request_with_timeout("ping", json!({}), Duration::from_secs(10))
            .await
            .map(|_| ())
    }

    pub fn is_alive(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }

    /// Exit code if the child process has already terminated.
    pub async fn child_exit_code(&self) -> Option<i32> {
        let mut guard = self.child.lock().await;
        let child = guard.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => status.code(),
            _ => None,
        }
    }

    pub fn subscribe_notifications(&self) -> broadcast::Receiver<Value> {
        self.notifications.subscribe()
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.writer.close().await;
        if let Some(task) = self.reader_task.lock().await.take() {
            task.abort();
        }
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.log.info(LogSource::Mcp, "MCP connection closed");
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

async fn reader_loop(
    mut reader: Box<dyn MessageReader>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    writer: Arc<dyn MessageWriter>,
    closed: Arc<AtomicBool>,
    log: ActivityLog,
    notifications: broadcast::Sender<Value>,
) {
    loop {
        match reader.next().await {
            Ok(Some(value)) => {
                let response: JsonRpcResponse = match serde_json::from_value(value.clone()) {
                    Ok(r) => r,
                    Err(e) => {
                        log.warn(
                            LogSource::Mcp,
                            format!("ignored malformed MCP message: {e}"),
                        );
                        continue;
                    }
                };

                let has_method = response.method.is_some();
                if !has_method {
                    if let Some(id) = response.id_u64() {
                        if let Some(sender) = pending.lock().await.remove(&id) {
                            let _ = sender.send(response);
                        } else {
                            log.debug(
                                LogSource::Mcp,
                                format!("response for unknown request id {id}"),
                            );
                        }
                    }
                    continue;
                }

                // Server → client request: answer so the server never blocks.
                if let Some(id) = response.id.clone() {
                    let method = response.method.clone().unwrap_or_default();
                    let reply = if method == "ping" {
                        json!({"jsonrpc":"2.0","id":id,"result":{}})
                    } else {
                        log.debug(
                            LogSource::Mcp,
                            format!("declining server request `{method}`"),
                        );
                        json!({
                            "jsonrpc":"2.0",
                            "id":id,
                            "error":{"code":-32601,"message":format!("Method not supported by TerCode: {method}")}
                        })
                    };
                    let _ = writer.send(&reply).await;
                    continue;
                }

                // Notification (no id): forward to subscribers.
                let _ = notifications.send(value);
            }
            Ok(None) => {
                closed.store(true, Ordering::SeqCst);
                log.warn(LogSource::Mcp, "MCP server closed the connection");
                // Wake every waiting caller so nothing hangs for the timeout.
                let mut guard = pending.lock().await;
                for (_, sender) in guard.drain() {
                    let _ = sender.send(JsonRpcResponse {
                        id: None,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32000,
                            message: "connection closed".into(),
                            data: None,
                        }),
                        method: None,
                    });
                }
                return;
            }
            Err(e) => {
                closed.store(true, Ordering::SeqCst);
                log.error(LogSource::Mcp, format!("MCP read failed: {e}"));
                let mut guard = pending.lock().await;
                for (_, sender) in guard.drain() {
                    let _ = sender.send(JsonRpcResponse {
                        id: None,
                        result: None,
                        error: Some(JsonRpcError {
                            code: -32000,
                            message: e.to_string(),
                            data: None,
                        }),
                        method: None,
                    });
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real in-process MCP server implementation used by the integration
    /// tests: it speaks the actual JSON-RPC protocol over pipes.
    pub(crate) struct FakeMcpServer {
        pub dir: tempfile::TempDir,
    }

    impl FakeMcpServer {
        pub fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let script = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"Fake Studio MCP","version":"9.9.9"}}}\n' "$id" ;;
    *'"method":"notifications/initialized"'*)
      : ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"script_read","description":"Read a script","inputSchema":{"type":"object","properties":{"studio_id":{"type":"string"},"path":{"type":"string"}},"required":["studio_id","path"]}},{"name":"list_roblox_studios","description":"List studios","inputSchema":{"type":"object","properties":{}}},{"name":"multi_edit","description":"Edit scripts","inputSchema":{"type":"object","properties":{"studio_id":{"type":"string"},"edits":{"type":"array"}},"required":["studio_id","edits"]}}]}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      case "$line" in
        *list_roblox_studios*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"[{\\"name\\":\\"MyGame\\",\\"studio_id\\":\\"studio-abc\\",\\"place_id\\":123}]"}]}}\n' "$id" ;;
        *script_read*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"print(\\"hello from studio\\")"}],"isError":false}}\n' "$id" ;;
        *multi_edit*)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"applied 1 edit"}],"isError":false}}\n' "$id" ;;
        *)
          printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}\n' "$id" ;;
      esac ;;
    *'"method":"ping"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
    *'"method":"fail_me"'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"bad params"}}\n' "$id" ;;
    *'"id"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done"#;
            let path = dir.path().join("fake_mcp.sh");
            std::fs::write(&path, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&path).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&path, perms).unwrap();
            }
            Self { dir }
        }

        pub async fn connect(&self, log: ActivityLog) -> Connection {
            let path = self.dir.path().join("fake_mcp.sh");
            let spawned = crate::mcp::transport::spawn_stdio(
                &crate::mcp::transport::StdioCommand::new("sh", vec![path.display().to_string()]),
                log,
            )
            .unwrap();
            Connection::open(spawned, ActivityLog::new()).await.unwrap()
        }
    }

    #[tokio::test]
    async fn handshake_discovers_tools() {
        let server = FakeMcpServer::new();
        let conn = server.connect(ActivityLog::new()).await;
        assert_eq!(conn.init.server_info.name, "Fake Studio MCP");
        assert_eq!(conn.init.protocol_version, "2025-06-18");
        assert_eq!(conn.tools.len(), 3);
        let names: Vec<_> = conn.tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"script_read"));
        conn.shutdown().await;
    }

    #[tokio::test]
    async fn calls_tools_and_reads_text() {
        let server = FakeMcpServer::new();
        let conn = server.connect(ActivityLog::new()).await;
        let result = conn
            .call_tool("script_read", json!({"studio_id":"s","path":"game.Workspace"}))
            .await
            .unwrap();
        assert!(result.to_model_text(1000).contains("hello from studio"));
        conn.shutdown().await;
    }

    #[tokio::test]
    async fn surfaces_jsonrpc_errors() {
        let server = FakeMcpServer::new();
        let conn = server.connect(ActivityLog::new()).await;
        let err = conn.request("fail_me", json!({})).await.unwrap_err();
        let text = err.to_string();
        assert!(text.contains("-32602"));
        assert!(text.contains("bad params"));
        conn.shutdown().await;
    }

    #[tokio::test]
    async fn ping_keeps_session_healthy() {
        let server = FakeMcpServer::new();
        let conn = server.connect(ActivityLog::new()).await;
        assert!(conn.is_alive());
        conn.ping().await.unwrap();
        conn.shutdown().await;
        assert!(!conn.is_alive());
    }

    #[tokio::test]
    async fn concurrent_requests_are_demultiplexed() {
        let server = FakeMcpServer::new();
        let conn = Arc::new(server.connect(ActivityLog::new()).await);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let conn = conn.clone();
            handles.push(tokio::spawn(async move {
                conn.call_tool("script_read", json!({"path":"x"})).await
            }));
        }
        for handle in handles {
            let result = handle.await.unwrap().unwrap();
            assert!(result.to_model_text(100).contains("hello from studio"));
        }
        conn.shutdown().await;
    }
}
