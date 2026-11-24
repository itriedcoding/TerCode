//! MCP transports: stdio (child process) and Streamable HTTP.
//!
//! Both are split into a writer and a reader so a single background task can
//! demultiplex responses by JSON-RPC id while tool calls run concurrently.

use crate::error::{Result, TerError};
use crate::logging::{ActivityLog, LogSource};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};

#[async_trait]
pub trait MessageWriter: Send + Sync {
    async fn send(&self, message: &Value) -> Result<()>;
    async fn close(&self) -> Result<()>;
    fn describe(&self) -> String;
    /// Called after `initialize` so HTTP can echo `Mcp-Protocol-Version`.
    async fn set_protocol_version(&self, _version: &str) {}
}

#[async_trait]
pub trait MessageReader: Send {
    async fn next(&mut self) -> Result<Option<Value>>;
}

pub struct SpawnedTransport {
    pub writer: Arc<dyn MessageWriter>,
    pub reader: Box<dyn MessageReader>,
    pub child: Option<Child>,
    pub description: String,
}

/// Everything needed to launch a stdio MCP server.
#[derive(Debug, Clone, PartialEq)]
pub struct StdioCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl StdioCommand {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            program: program.into(),
            args,
            cwd: None,
        }
    }

    pub fn describe(&self) -> String {
        let mut s = shell_quote(&self.program);
        for arg in &self.args {
            s.push(' ');
            s.push_str(&shell_quote(arg));
        }
        s
    }
}

fn shell_quote(value: &str) -> String {
    if value.contains(' ') && !value.starts_with('"') {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

/// Spawns a stdio MCP server. On Windows the process is created without a
/// console window so launching Studio tooling never flashes a terminal.
pub fn spawn_stdio(command: &StdioCommand, log: ActivityLog) -> Result<SpawnedTransport> {
    let mut cmd = Command::new(&command.program);
    cmd.args(&command.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &command.cwd {
        cmd.current_dir(cwd);
    }
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn().map_err(|e| {
        TerError::Mcp(format!(
            "could not start MCP server `{}`: {e}. Check the command in Settings → MCP.",
            command.describe()
        ))
    })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| TerError::Mcp("MCP server stdin was not captured".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| TerError::Mcp("MCP server stdout was not captured".into()))?;
    let stderr = child.stderr.take();

    if let Some(stderr) = stderr {
        let log = log.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let trimmed = line.trim_end().to_string();
                if trimmed.is_empty() {
                    continue;
                }
                // Roblox's server logs human-facing text to stderr.
                log.debug(LogSource::Mcp, trimmed);
            }
        });
    }

    log.info(
        LogSource::Mcp,
        format!("started MCP server: {}", command.describe()),
    );

    Ok(SpawnedTransport {
        writer: Arc::new(StdioWriter {
            stdin: Mutex::new(stdin),
            description: command.describe(),
        }),
        reader: Box::new(StdioReader {
            stdout: BufReader::new(stdout),
            log: log.clone(),
        }),
        child: Some(child),
        description: command.describe(),
    })
}

struct StdioWriter {
    stdin: Mutex<ChildStdin>,
    description: String,
}

#[async_trait]
impl MessageWriter for StdioWriter {
    async fn send(&self, message: &Value) -> Result<()> {
        // MCP stdio framing: one JSON object per line, UTF-8, no length prefix.
        let mut line = serde_json::to_vec(message)
            .map_err(|e| TerError::json("MCP message", e))?;
        line.push(b'\n');
        let mut guard = self.stdin.lock().await;
        guard
            .write_all(&line)
            .await
            .map_err(|e| TerError::Mcp(format!("could not write to MCP server: {e}")))?;
        guard
            .flush()
            .await
            .map_err(|e| TerError::Mcp(format!("could not flush MCP server stdin: {e}")))?;
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        let mut guard = self.stdin.lock().await;
        let _ = guard.shutdown().await;
        Ok(())
    }

    fn describe(&self) -> String {
        self.description.clone()
    }
}

struct StdioReader {
    stdout: BufReader<ChildStdout>,
    log: ActivityLog,
}

#[async_trait]
impl MessageReader for StdioReader {
    async fn next(&mut self) -> Result<Option<Value>> {
        let mut buf = String::new();
        loop {
            buf.clear();
            let read = self
                .stdout
                .read_line(&mut buf)
                .await
                .map_err(|e| TerError::Mcp(format!("could not read from MCP server: {e}")))?;
            if read == 0 {
                return Ok(None); // EOF: server exited.
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(trimmed) {
                Ok(value) => return Ok(Some(value)),
                Err(e) => {
                    // Never let stray output (banners, warnings) break the session.
                    self.log.warn(
                        LogSource::Mcp,
                        format!("ignored non-JSON output from MCP server ({e}): {}", truncate(trimmed, 200)),
                    );
                }
            }
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// Connects to a Streamable HTTP MCP endpoint.
///
/// Requests are POSTed; the server may answer with `application/json` or a
/// `text/event-stream` containing the response. A `Mcp-Session-Id` returned by
/// `initialize` is echoed on later requests, as the spec requires.
pub async fn connect_http(url: &str, log: ActivityLog) -> Result<SpawnedTransport> {
    let client = reqwest::Client::builder()
        .user_agent(crate::USER_AGENT)
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| TerError::Mcp(format!("could not build HTTP client: {e}")))?;

    let (tx, rx) = mpsc::unbounded_channel();
    let writer = Arc::new(HttpWriter {
        client,
        url: url.to_string(),
        session_id: Mutex::new(None),
        protocol_version: Mutex::new(None),
        tx,
        log: log.clone(),
    });

    Ok(SpawnedTransport {
        writer: writer.clone(),
        reader: Box::new(HttpReader { rx }),
        child: None,
        description: format!("http: {url}"),
    })
}

struct HttpWriter {
    client: reqwest::Client,
    url: String,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<Option<String>>,
    tx: mpsc::UnboundedSender<Value>,
    log: ActivityLog,
}

#[async_trait]
impl MessageWriter for HttpWriter {
    async fn send(&self, message: &Value) -> Result<()> {
        let is_notification = message.get("id").is_none();
        let mut request = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .json(message);
        if let Some(session) = self.session_id.lock().await.clone() {
            request = request.header("Mcp-Session-Id", session);
        }
        if let Some(version) = self.protocol_version.lock().await.clone() {
            request = request.header("Mcp-Protocol-Version", version);
        }

        let response = request
            .send()
            .await
            .map_err(|e| TerError::Mcp(format!("HTTP MCP request failed: {e}")))?;

        if let Some(session) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session_id.lock().await = Some(session.to_string());
        }

        let status = response.status();
        if status == reqwest::StatusCode::ACCEPTED {
            return Ok(());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(TerError::Mcp(format!(
                "HTTP MCP server returned {status}: {}",
                truncate(body.trim(), 300)
            )));
        }

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = response
            .text()
            .await
            .map_err(|e| TerError::Mcp(format!("could not read MCP response: {e}")))?;

        if content_type.contains("text/event-stream") {
            let mut buf = body.into_bytes();
            for payload in crate::ai::sse::drain_events(&mut buf) {
                if payload.trim().is_empty() || crate::ai::sse::is_done(&payload) {
                    continue;
                }
                match serde_json::from_str::<Value>(&payload) {
                    Ok(value) => {
                        let _ = self.tx.send(value);
                    }
                    Err(e) => self
                        .log
                        .warn(LogSource::Mcp, format!("unparsable SSE payload: {e}")),
                }
            }
        } else if !body.trim().is_empty() {
            match serde_json::from_str::<Value>(&body) {
                Ok(value) => {
                    let _ = self.tx.send(value);
                }
                Err(e) => {
                    return Err(TerError::Mcp(format!(
                        "MCP server returned non-JSON body ({e}): {}",
                        truncate(body.trim(), 200)
                    )))
                }
            }
        } else if !is_notification {
            return Err(TerError::Mcp(
                "MCP server returned an empty response body".into(),
            ));
        }
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        if let Some(session) = self.session_id.lock().await.clone() {
            let _ = self
                .client
                .delete(&self.url)
                .header("Mcp-Session-Id", session)
                .send()
                .await;
        }
        Ok(())
    }

    fn describe(&self) -> String {
        format!("http: {}", self.url)
    }

    async fn set_protocol_version(&self, version: &str) {
        *self.protocol_version.lock().await = Some(version.to_string());
    }
}

struct HttpReader {
    rx: mpsc::UnboundedReceiver<Value>,
}

#[async_trait]
impl MessageReader for HttpReader {
    async fn next(&mut self) -> Result<Option<Value>> {
        Ok(self.rx.recv().await)
    }
}

/// Expands `%LOCALAPPDATA%`-style variables in a user-supplied command.
pub fn expand_env(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '%' {
            if let Some(end) = bytes[i + 1..].iter().position(|c| *c == '%') {
                let name: String = bytes[i + 1..i + 1 + end].iter().collect();
                if let Ok(value) = std::env::var(&name) {
                    out.push_str(&value);
                    i += end + 2;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// True when `path` looks like an existing file.
pub fn file_exists(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_environment_variables() {
        std::env::set_var("TERCODE_TEST_DIR", r"C:\Users\Test\AppData\Local");
        assert_eq!(
            expand_env(r"%TERCODE_TEST_DIR%\Roblox\mcp.bat"),
            r"C:\Users\Test\AppData\Local\Roblox\mcp.bat"
        );
        // Unknown variables are left untouched rather than silently dropped.
        assert_eq!(expand_env("%NOT_SET_ANYWHERE%\\x"), "%NOT_SET_ANYWHERE%\\x");
        assert_eq!(expand_env("plain"), "plain");
    }

    #[test]
    fn quotes_paths_with_spaces() {
        let cmd = StdioCommand::new(
            r"C:\Program Files\node.exe",
            vec!["--stdio".into()],
        );
        assert!(cmd.describe().starts_with('"'));
        assert!(cmd.describe().contains("--stdio"));
    }

    #[tokio::test]
    async fn stdio_roundtrip_with_real_child_process() {
        // A real process speaking line-delimited JSON-RPC: proves the transport
        // framing works end to end (not a mock).
        let script = r#"while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  if [ -n "$id" ]; then
    printf '{"jsonrpc":"2.0","id":%s,"result":{"echo":true}}\n' "$id"
  fi
done"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("echo.sh");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        let log = ActivityLog::new();
        let spawned = spawn_stdio(
            &StdioCommand::new("sh", vec![path.display().to_string()]),
            log,
        )
        .unwrap();
        spawned
            .writer
            .send(&serde_json::json!({"jsonrpc":"2.0","id":7,"method":"ping","params":{}}))
            .await
            .unwrap();
        let mut reader = spawned.reader;
        let message = reader.next().await.unwrap().expect("response");
        assert_eq!(message["id"], 7);
        assert_eq!(message["result"]["echo"], true);
    }

    #[tokio::test]
    async fn stdio_reports_missing_binary() {
        let log = ActivityLog::new();
        let err = match spawn_stdio(
            &StdioCommand::new("tercode-definitely-not-a-real-binary", vec![]),
            log,
        ) {
            Ok(_) => panic!("expected the spawn to fail"),
            Err(e) => e,
        };
        assert!(matches!(err, TerError::Mcp(_)));
        assert!(err.to_string().contains("could not start"));
    }
}
