//! MCP connection manager: automatic connect, reconnect, status, tool routing.

pub mod connection;
pub mod discovery;
pub mod protocol;
pub mod transport;

pub use connection::Connection;
pub use discovery::{DiscoveryReport, Launcher, StudioInstall};
pub use protocol::{CallToolResult, McpTool};

use crate::error::{Result, TerError};
use crate::logging::{ActivityLog, LogLevel, LogSource};
use crate::settings::{McpSettings, McpTransportKind};
use serde::Serialize;
use serde_json::{Map, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex, RwLock};

const HEALTH_INTERVAL: Duration = Duration::from_secs(5);
const MAX_BACKOFF_SECS: u64 = 30;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum McpStatus {
    Disconnected,
    Connecting,
    Connected {
        server: String,
        version: String,
        tool_count: usize,
        transport: String,
        #[serde(rename = "studioId")]
        studio_id: Option<String>,
    },
    Reconnecting {
        attempt: u32,
        #[serde(rename = "nextRetrySecs")]
        next_retry_secs: u64,
    },
    Error {
        message: String,
        hint: Option<String>,
    },
}

impl McpStatus {
    pub fn is_connected(&self) -> bool {
        matches!(self, McpStatus::Connected { .. })
    }

    pub fn label(&self) -> &'static str {
        match self {
            McpStatus::Disconnected => "Disconnected",
            McpStatus::Connecting => "Connecting",
            McpStatus::Connected { .. } => "Connected",
            McpStatus::Reconnecting { .. } => "Reconnecting",
            McpStatus::Error { .. } => "Error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSummary {
    pub name: String,
    pub model_name: String,
    pub title: Option<String>,
    pub description: String,
    pub required: Vec<String>,
    pub properties: Vec<String>,
    pub significant: bool,
}

impl From<&McpTool> for ToolSummary {
    fn from(tool: &McpTool) -> Self {
        Self {
            name: tool.name.clone(),
            model_name: tool.model_name(),
            title: tool.title.clone(),
            description: tool.description.clone().unwrap_or_default(),
            required: tool
                .input_schema
                .get("required")
                .and_then(|r| r.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            properties: tool.schema_properties(),
            significant: protocol::is_significant_tool(&tool.name),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum McpEvent {
    Status {
        status: McpStatus,
    },
    Tools {
        tools: Vec<ToolSummary>,
    },
    Activity {
        tool: String,
        phase: &'static str,
        detail: Option<String>,
        #[serde(rename = "durationMs")]
        duration_ms: Option<u64>,
        ok: Option<bool>,
    },
}

/// Owns the live MCP session and keeps it healthy.
pub struct McpManager {
    connection: RwLock<Option<Arc<Connection>>>,
    status: RwLock<McpStatus>,
    settings: RwLock<McpSettings>,
    log: ActivityLog,
    events: broadcast::Sender<McpEvent>,
    report: RwLock<Option<DiscoveryReport>>,
    studio_id: RwLock<Option<String>>,
    /// Serialises connection attempts so auto-connect and manual reconnect
    /// cannot race each other.
    connect_lock: Mutex<()>,
    supervisor: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutting_down: Arc<AtomicBool>,
}

impl McpManager {
    pub fn new(log: ActivityLog, settings: McpSettings) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        Arc::new(Self {
            connection: RwLock::new(None),
            status: RwLock::new(McpStatus::Disconnected),
            settings: RwLock::new(settings),
            log,
            events,
            report: RwLock::new(None),
            studio_id: RwLock::new(None),
            connect_lock: Mutex::new(()),
            supervisor: Mutex::new(None),
            shutting_down: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<McpEvent> {
        self.events.subscribe()
    }

    pub async fn status(&self) -> McpStatus {
        self.status.read().await.clone()
    }

    pub async fn tools(&self) -> Vec<McpTool> {
        match self.connection.read().await.as_ref() {
            Some(conn) => conn.tools.clone(),
            None => Vec::new(),
        }
    }

    pub async fn tool_summaries(&self) -> Vec<ToolSummary> {
        self.tools().await.iter().map(ToolSummary::from).collect()
    }

    pub async fn settings(&self) -> McpSettings {
        self.settings.read().await.clone()
    }

    pub async fn update_settings(&self, settings: McpSettings) {
        *self.settings.write().await = settings;
    }

    pub async fn report(&self) -> Option<DiscoveryReport> {
        self.report.read().await.clone()
    }

    async fn set_status(&self, status: McpStatus) {
        let mut guard = self.status.write().await;
        if *guard == status {
            return;
        }
        *guard = status.clone();
        let _ = self.events.send(McpEvent::Status { status });
    }

    async fn emit_tools(&self) {
        let tools = self.tool_summaries().await;
        let _ = self.events.send(McpEvent::Tools { tools });
    }

    /// Runs discovery, then connects using the resolved launcher.
    pub async fn connect(&self) -> Result<()> {
        let _guard = self.connect_lock.lock().await;
        self.connect_locked().await
    }

    async fn connect_locked(&self) -> Result<()> {
        let settings = self.settings.read().await.clone();

        // Tear down any previous session first.
        self.drop_connection().await;

        self.set_status(McpStatus::Connecting).await;
        self.log.info(LogSource::Mcp, "connecting to Roblox Studio MCP…");

        let http_url = if settings.transport == McpTransportKind::Http {
            Some(settings.http_url.as_str())
        } else {
            None
        };
        let custom_command = if settings.command.trim().is_empty() {
            None
        } else {
            Some(settings.command.trim())
        };

        let report = discovery::discover(custom_command, &settings.args, http_url);
        for note in &report.notes {
            self.log.info(LogSource::Studio, note.clone());
        }
        *self.report.write().await = Some(report.clone());

        let Some(launcher) = report.launcher.clone() else {
            let message = if !report.studio_installed {
                "Roblox Studio was not found on this machine".to_string()
            } else {
                "The Roblox Studio MCP server is not available".to_string()
            };
            let hint = report.notes.first().cloned();
            self.log.error(LogSource::Mcp, message.clone());
            self.set_status(McpStatus::Error {
                message,
                hint: hint.clone(),
            })
            .await;
            return Err(TerError::McpNotFound(
                hint.unwrap_or_else(|| {
                    "Enable Studio as an MCP server, then press Reconnect.".into()
                }),
            ));
        };

        self.log.info(
            LogSource::Mcp,
            format!("using {}", launcher.describe()),
        );

        let spawned = match &launcher {
            Launcher::Http { url } => transport::connect_http(url, self.log.clone()).await?,
            other => {
                let command = discovery::launcher_command(other).ok_or_else(|| {
                    TerError::Mcp("this launcher does not produce a stdio command".into())
                })?;
                transport::spawn_stdio(&command, self.log.clone())?
            }
        };

        let connection = match Connection::open(spawned, self.log.clone()).await {
            Ok(conn) => Arc::new(conn),
            Err(e) => {
                self.log.error(LogSource::Mcp, e.to_string());
                self.set_status(McpStatus::Error {
                    message: e.to_string(),
                    hint: Some(
                        "If Studio is open, enable Assistant → ⋯ → Manage MCP Servers → Enable Studio as MCP server."
                            .into(),
                    ),
                })
                .await;
                return Err(e);
            }
        };

        // Resolve the Studio instance so the AI can target it explicitly.
        let studio_id = self.resolve_studio_id(&connection).await;
        if let Some(id) = &studio_id {
            *self.studio_id.write().await = Some(id.clone());
            self.log.info(LogSource::Studio, format!("active Studio: {id}"));
        }

        let status = McpStatus::Connected {
            server: if connection.init.server_info.name.is_empty() {
                "Roblox Studio MCP".to_string()
            } else {
                connection.init.server_info.name.clone()
            },
            version: connection.init.server_info.version.clone(),
            tool_count: connection.tools.len(),
            transport: connection.description.clone(),
            studio_id: studio_id.clone(),
        };

        *self.connection.write().await = Some(connection);
        self.set_status(status).await;
        self.emit_tools().await;
        Ok(())
    }

    async fn drop_connection(&self) {
        if let Some(conn) = self.connection.write().await.take() {
            conn.shutdown().await;
        }
        *self.studio_id.write().await = None;
    }

    pub async fn disconnect(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.drop_connection().await;
        self.set_status(McpStatus::Disconnected).await;
        self.emit_tools().await;
    }

    /// Reconnect on demand (the "Reconnect" button).
    pub async fn reconnect(&self) -> Result<()> {
        self.shutting_down.store(false, Ordering::SeqCst);
        self.connect().await
    }

    async fn resolve_studio_id(&self, connection: &Connection) -> Option<String> {
        let configured = self.settings.read().await.studio_id.trim().to_string();
        if !configured.is_empty() {
            return Some(configured);
        }
        let tool = connection
            .tools
            .iter()
            .find(|t| t.name == "list_roblox_studios")?;
        let result = connection
            .call_tool(&tool.name, Value::Object(Map::new()))
            .await
            .ok()?;
        protocol::extract_studio_id(&result)
    }

    /// Calls an MCP tool, injecting the `studio_id` the Roblox server requires.
    pub async fn call_tool(&self, name: &str, mut arguments: Value) -> Result<CallToolResult> {
        let connection = {
            let guard = self.connection.read().await;
            guard.clone()
        };
        let Some(connection) = connection else {
            return Err(TerError::Mcp(
                "Roblox Studio is not connected. Start Studio and press Reconnect.".into(),
            ));
        };
        if !connection.is_alive() {
            return Err(TerError::Mcp(
                "The MCP connection dropped. Press Reconnect to restore it.".into(),
            ));
        }

        let tool = connection
            .tools
            .iter()
            .find(|t| t.name == name)
            .cloned()
            .ok_or_else(|| {
                TerError::Mcp(format!(
                    "the MCP server does not expose a tool named `{name}`"
                ))
            })?;

        if let Value::Object(map) = &mut arguments {
            self.inject_studio_id(&connection, &tool, map).await;
        }

        let started = Instant::now();
        let _ = self.events.send(McpEvent::Activity {
            tool: name.to_string(),
            phase: "start",
            detail: Some(summarize_arguments(&arguments)),
            duration_ms: None,
            ok: None,
        });
        self.log.push_detailed(
            LogLevel::Info,
            LogSource::Mcp,
            format!("→ {name}"),
            Some(summarize_arguments(&arguments)),
        );

        let outcome = connection.call_tool(name, arguments).await;
        let duration = started.elapsed().as_millis() as u64;

        match &outcome {
            Ok(result) => {
                let detail = result.to_model_text(400);
                let level = if result.is_error {
                    LogLevel::Warn
                } else {
                    LogLevel::Info
                };
                self.log
                    .push_detailed(level, LogSource::Mcp, format!("← {name} ({duration}ms)"), Some(detail));
                let _ = self.events.send(McpEvent::Activity {
                    tool: name.to_string(),
                    phase: "finish",
                    detail: None,
                    duration_ms: Some(duration),
                    ok: Some(!result.is_error),
                });
            }
            Err(e) => {
                self.log.push_detailed(
                    LogLevel::Error,
                    LogSource::Mcp,
                    format!("✗ {name} ({duration}ms)"),
                    Some(e.to_string()),
                );
                let _ = self.events.send(McpEvent::Activity {
                    tool: name.to_string(),
                    phase: "finish",
                    detail: Some(e.to_string()),
                    duration_ms: Some(duration),
                    ok: Some(false),
                });
            }
        }

        outcome
    }

    async fn inject_studio_id(
        &self,
        connection: &Connection,
        tool: &McpTool,
        arguments: &mut Map<String, Value>,
    ) {
        if !tool.schema_properties().iter().any(|p| p == "studio_id") {
            return;
        }
        if arguments
            .get("studio_id")
            .and_then(|v| v.as_str())
            .map(|v| !v.is_empty())
            .unwrap_or(false)
        {
            return;
        }
        if let Some(id) = self.studio_id.read().await.clone() {
            arguments.insert("studio_id".into(), Value::String(id));
            return;
        }
        if let Some(id) = self.resolve_studio_id(connection).await {
            *self.studio_id.write().await = Some(id.clone());
            arguments.insert("studio_id".into(), Value::String(id));
        }
    }

    pub async fn refresh_tools(&self) -> Result<Vec<ToolSummary>> {
        let connection = {
            let guard = self.connection.read().await;
            guard.clone()
        };
        let Some(connection) = connection else {
            return Err(TerError::Mcp("not connected".into()));
        };
        // Connection::refresh_tools needs &mut; rebuild the tool list instead.
        let tools = connection.request("tools/list", Value::Object(Map::new())).await?;
        let parsed: protocol::ToolsListResult = serde_json::from_value(tools)
            .map_err(|e| TerError::Mcp(format!("invalid tools/list result: {e}")))?;
        let summaries: Vec<ToolSummary> = parsed.tools.iter().map(ToolSummary::from).collect();
        let _ = self.events.send(McpEvent::Tools {
            tools: summaries.clone(),
        });
        Ok(summaries)
    }

    /// Starts the background supervisor: auto-connect at launch, health checks,
    /// and automatic reconnection when Studio restarts.
    pub async fn start_supervisor(self: &Arc<Self>) {
        let mut guard = self.supervisor.lock().await;
        if guard.is_some() {
            return;
        }
        let manager = self.clone();
        let handle = tokio::spawn(async move {
            let mut attempt: u32 = 0;
            let mut announced_wait = false;
            loop {
                if manager.shutting_down.load(Ordering::SeqCst) {
                    tokio::time::sleep(HEALTH_INTERVAL).await;
                    continue;
                }
                let settings = manager.settings.read().await.clone();
                let connected = manager.status.read().await.is_connected();

                if connected {
                    let connection = manager.connection.read().await.clone();
                    let healthy = match connection {
                        Some(conn) => conn.is_alive() && conn.ping().await.is_ok(),
                        None => false,
                    };
                    if healthy {
                        attempt = 0;
                        announced_wait = false;
                    } else {
                        manager
                            .log
                            .warn(LogSource::Mcp, "MCP health check failed; reconnecting");
                        manager.drop_connection().await;
                        manager.set_status(McpStatus::Disconnected).await;
                        manager.emit_tools().await;
                    }
                    tokio::time::sleep(HEALTH_INTERVAL).await;
                    continue;
                }

                if !settings.auto_connect || !settings.auto_reconnect {
                    tokio::time::sleep(HEALTH_INTERVAL).await;
                    continue;
                }

                attempt += 1;
                let delay = (1u64 << attempt.min(5)).min(MAX_BACKOFF_SECS);
                if !announced_wait {
                    manager
                        .log
                        .info(LogSource::Mcp, "waiting for Roblox Studio MCP server…");
                    announced_wait = true;
                }
                // Do not overwrite a detailed error with a reconnect spinner.
                if !matches!(*manager.status.read().await, McpStatus::Error { .. }) {
                    manager
                        .set_status(McpStatus::Reconnecting {
                            attempt,
                            next_retry_secs: delay,
                        })
                        .await;
                }
                tokio::time::sleep(Duration::from_secs(delay.min(10))).await;
                if manager.shutting_down.load(Ordering::SeqCst) {
                    continue;
                }
                match manager.connect().await {
                    Ok(()) => {
                        attempt = 0;
                        announced_wait = false;
                        manager.log.info(LogSource::Mcp, "MCP connection established");
                    }
                    Err(e) => {
                        manager
                            .log
                            .debug(LogSource::Mcp, format!("connect attempt failed: {e}"));
                    }
                }
                tokio::time::sleep(HEALTH_INTERVAL).await;
            }
        });
        *guard = Some(handle);
    }

    pub async fn stop_supervisor(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        if let Some(handle) = self.supervisor.lock().await.take() {
            handle.abort();
        }
    }

    /// Full diagnostics for the MCP panel.
    pub async fn diagnostics(&self) -> Diagnostics {
        let status = self.status().await;
        let report = self.report().await;
        let (server, protocol, tool_count) = {
            let guard = self.connection.read().await;
            match guard.as_ref() {
                Some(conn) => (
                    Some(conn.init.server_info.clone()),
                    Some(conn.init.protocol_version.clone()),
                    conn.tools.len(),
                ),
                None => (None, None, 0),
            }
        };
        Diagnostics {
            status,
            report,
            server,
            protocol_version: protocol,
            tool_count,
            recent_logs: self.log.snapshot(80),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostics {
    pub status: McpStatus,
    pub report: Option<DiscoveryReport>,
    pub server: Option<protocol::ServerInfo>,
    pub protocol_version: Option<String>,
    pub tool_count: usize,
    pub recent_logs: Vec<crate::logging::LogEntry>,
}

fn summarize_arguments(arguments: &Value) -> String {
    let text = serde_json::to_string(arguments).unwrap_or_default();
    if text.chars().count() <= 300 {
        text
    } else {
        text.chars().take(300).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manager() -> Arc<McpManager> {
        McpManager::new(ActivityLog::new(), McpSettings::default())
    }

    #[tokio::test]
    async fn starts_disconnected() {
        let m = manager();
        assert_eq!(m.status().await, McpStatus::Disconnected);
        assert!(m.tools().await.is_empty());
    }

    #[tokio::test]
    async fn call_without_connection_is_actionable() {
        let m = manager();
        let err = m
            .call_tool("script_read", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Reconnect"));
    }

    #[tokio::test]
    async fn status_events_are_broadcast() {
        let m = manager();
        let mut rx = m.subscribe();
        m.set_status(McpStatus::Connecting).await;
        let event = rx.recv().await.unwrap();
        match event {
            McpEvent::Status { status } => assert_eq!(status, McpStatus::Connecting),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn connecting_without_studio_reports_error_with_hint() {
        let m = manager();
        let err = m.connect().await.unwrap_err();
        assert!(
            matches!(err, TerError::McpNotFound(_)),
            "unexpected: {err}"
        );
        match m.status().await {
            McpStatus::Error { message, hint } => {
                assert!(!message.is_empty());
                assert!(hint.is_some());
            }
            other => panic!("expected error status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_summaries_flag_significant_tools() {
        let tool = McpTool {
            name: "multi_edit".into(),
            title: None,
            description: Some("Apply edits".into()),
            input_schema: serde_json::json!({"type":"object","properties":{"studio_id":{"type":"string"}},"required":["studio_id"]}),
            annotations: None,
        };
        let summary = ToolSummary::from(&tool);
        assert!(summary.significant);
        assert_eq!(summary.required, vec!["studio_id".to_string()]);
        assert_eq!(summary.model_name, "multi_edit");
    }

    #[test]
    fn status_labels_match_ui() {
        assert_eq!(McpStatus::Disconnected.label(), "Disconnected");
        assert_eq!(
            McpStatus::Connected {
                server: "s".into(),
                version: "1".into(),
                tool_count: 3,
                transport: "stdio".into(),
                studio_id: None
            }
            .label(),
            "Connected"
        );
    }
}
