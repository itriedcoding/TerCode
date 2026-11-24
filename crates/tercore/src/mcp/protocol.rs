//! Model Context Protocol types (JSON-RPC 2.0).
//!
//! Spec: <https://modelcontextprotocol.io/specification/2025-06-18>. The
//! Roblox Studio MCP server implements stdio transport and requires an
//! `initialize` → `notifications/initialized` handshake before tool calls.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Versions this client can speak if the server negotiates an older one.
pub const SUPPORTED_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    pub params: Value,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct JsonRpcError {
    #[serde(default)]
    pub code: i64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
    #[serde(default)]
    pub method: Option<String>,
}

impl JsonRpcResponse {
    pub fn id_u64(&self) -> Option<u64> {
        match self.id.as_ref()? {
            Value::Number(n) => n.as_u64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ServerInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitializeResult {
    #[serde(default, rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default, rename = "serverInfo")]
    pub server_info: ServerInfo,
    #[serde(default)]
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: Option<Value>,
}

impl McpTool {
    /// Name the model sees. OpenAI requires `^[a-zA-Z0-9_-]{1,64}$`.
    pub fn model_name(&self) -> String {
        sanitize_tool_name(&self.name)
    }

    pub fn schema_properties(&self) -> Vec<String> {
        self.input_schema
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub fn requires_param(&self, name: &str) -> bool {
        self.input_schema
            .get("required")
            .and_then(|r| r.as_array())
            .map(|a| a.iter().any(|v| v.as_str() == Some(name)))
            .unwrap_or(false)
    }

    /// Human summary for the tool explorer.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(title) = &self.title {
            parts.push(title.clone());
        }
        if let Some(desc) = &self.description {
            let first = desc.lines().next().unwrap_or_default().trim();
            if !first.is_empty() {
                parts.push(first.chars().take(160).collect());
            }
        }
        parts.join(" — ")
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolsListResult {
    #[serde(default)]
    pub tools: Vec<McpTool>,
    #[serde(default, rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    Image {
        #[serde(default)]
        data: String,
        #[serde(default, rename = "mimeType")]
        mime_type: String,
    },
    Resource {
        #[serde(default)]
        resource: Value,
    },
    Audio {
        #[serde(default)]
        data: String,
        #[serde(default, rename = "mimeType")]
        mime_type: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct CallToolResult {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(default, rename = "isError")]
    pub is_error: bool,
    #[serde(default, rename = "structuredContent")]
    pub structured: Option<Value>,
}

impl CallToolResult {
    /// Flattens the result into text for the model. Images are described rather
    /// than inlined so the context stays small.
    pub fn to_model_text(&self, max_chars: usize) -> String {
        let mut out = String::new();
        for block in &self.content {
            match block {
                ContentBlock::Text { text } => {
                    out.push_str(text);
                    out.push('\n');
                }
                ContentBlock::Image { mime_type, .. } => {
                    out.push_str(&format!("[image: {mime_type}]\n"));
                }
                ContentBlock::Audio { mime_type, .. } => {
                    out.push_str(&format!("[audio: {mime_type}]\n"));
                }
                ContentBlock::Resource { resource } => {
                    out.push_str(&format!("[resource: {}]\n", compact(resource, 400)));
                }
                ContentBlock::Unknown => out.push_str("[unsupported content block]\n"),
            }
        }
        if out.trim().is_empty() {
            if let Some(structured) = &self.structured {
                out.push_str(&compact(structured, max_chars));
            }
        }
        if self.is_error {
            out = format!("TOOL ERROR: {out}");
        }
        truncate(&out, max_chars)
    }
}

fn compact(value: &Value, max: usize) -> String {
    truncate(&serde_json::to_string(value).unwrap_or_default(), max)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n… [truncated]");
    out
}

/// Converts an MCP tool name to an OpenAI-safe function name.
pub fn sanitize_tool_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("tool");
    }
    if out.len() > 64 {
        out.truncate(64);
    }
    out
}

/// Builds the `initialize` params this client sends.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "roots": { "listChanged": false },
            "sampling": {}
        },
        "clientInfo": {
            "name": "TerCode",
            "title": "TerCode — AI Roblox Studio companion",
            "version": crate::APP_VERSION
        }
    })
}

/// Extracts the first Studio instance id from a `list_roblox_studios` result.
///
/// The server returns text content containing JSON. Field naming has changed
/// across Studio releases (`studio_id`, `studioId`, `id`), so all are accepted.
pub fn extract_studio_id(result: &CallToolResult) -> Option<String> {
    let text = result.to_model_text(200_000);
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    find_studio_id(&value)
}

fn find_studio_id(value: &Value) -> Option<String> {
    match value {
        Value::Array(items) => items.iter().find_map(find_studio_id),
        Value::Object(map) => {
            for key in ["studio_id", "studioId", "studioID", "id"] {
                if let Some(Value::String(id)) = map.get(key) {
                    if !id.is_empty() {
                        return Some(id.clone());
                    }
                }
            }
            // Nested shapes: {"studios":[...]} or {"data":{"studios":[...]}}.
            for key in ["studios", "data", "result", "instances"] {
                if let Some(inner) = map.get(key) {
                    if let Some(found) = find_studio_id(inner) {
                        return Some(found);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Tools whose execution changes the project in a way the user should approve.
pub fn is_significant_tool(name: &str) -> bool {
    const EXACT: [&str; 9] = [
        "multi_edit",
        "save_place",
        "run_code",
        "insert_model",
        "start_stop_play",
        "run_script_in_play_mode",
        "upload_image",
        "open_cloud_invoke",
        "execute_lua",
    ];
    if EXACT.contains(&name) {
        return true;
    }
    name.starts_with("generate_")
        || name.contains("delete")
        || name.contains("publish")
        || name.contains("upload")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_with(text: &str) -> CallToolResult {
        CallToolResult {
            content: vec![ContentBlock::Text { text: text.into() }],
            is_error: false,
            structured: None,
        }
    }

    #[test]
    fn sanitizes_tool_names() {
        assert_eq!(sanitize_tool_name("script_read"), "script_read");
        assert_eq!(sanitize_tool_name("roblox.studio/read"), "roblox_studio_read");
        assert_eq!(sanitize_tool_name(""), "tool");
        assert_eq!(sanitize_tool_name(&"a".repeat(100)).len(), 64);
    }

    #[test]
    fn parses_initialize_result() {
        let raw = r#"{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},
            "serverInfo":{"name":"Roblox Studio MCP","version":"1.2.3"}}"#;
        let parsed: InitializeResult = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.protocol_version, "2025-06-18");
        assert_eq!(parsed.server_info.name, "Roblox Studio MCP");
    }

    #[test]
    fn detects_required_studio_id() {
        let tool = McpTool {
            name: "script_read".into(),
            title: None,
            description: None,
            input_schema: json!({
                "type":"object",
                "properties":{"studio_id":{"type":"string"},"path":{"type":"string"}},
                "required":["studio_id","path"]
            }),
            annotations: None,
        };
        assert!(tool.requires_param("studio_id"));
        assert!(!tool.requires_param("path_extra"));
        assert_eq!(tool.schema_properties().len(), 2);
    }

    #[test]
    fn extracts_studio_id_from_text_content() {
        let result = result_with(r#"[{"name":"MyGame","studio_id":"abc-123","place_id":987}]"#);
        assert_eq!(extract_studio_id(&result).as_deref(), Some("abc-123"));
    }

    #[test]
    fn extracts_studio_id_from_nested_object() {
        let result = result_with(r#"{"studios":[{"studioId":"xyz"}]}"#);
        assert_eq!(extract_studio_id(&result).as_deref(), Some("xyz"));
    }

    #[test]
    fn no_studio_id_when_empty() {
        let result = result_with("[]");
        assert!(extract_studio_id(&result).is_none());
    }

    #[test]
    fn flattens_content_and_marks_errors() {
        let result = CallToolResult {
            content: vec![
                ContentBlock::Text { text: "line one".into() },
                ContentBlock::Image { data: "AAA".into(), mime_type: "image/png".into() },
            ],
            is_error: true,
            structured: None,
        };
        let text = result.to_model_text(1000);
        assert!(text.starts_with("TOOL ERROR:"));
        assert!(text.contains("line one"));
        assert!(text.contains("[image: image/png]"));
    }

    #[test]
    fn truncates_long_output() {
        let result = result_with(&"x".repeat(500));
        let text = result.to_model_text(100);
        assert!(text.contains("[truncated]"));
        assert!(text.chars().count() < 140);
    }

    #[test]
    fn flags_significant_tools() {
        assert!(is_significant_tool("multi_edit"));
        assert!(is_significant_tool("generate_mesh"));
        assert!(is_significant_tool("save_place"));
        assert!(!is_significant_tool("script_read"));
        assert!(!is_significant_tool("list_roblox_studios"));
    }
}
