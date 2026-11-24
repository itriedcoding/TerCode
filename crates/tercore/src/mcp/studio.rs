//! Studio operations layered on the documented built-in MCP tools.
//!
//! Roblox documents the tool names and their arguments (`start_stop_play`
//! takes `mode`, `screen_capture` returns image data). Nothing here invents an
//! endpoint: every call first checks that the connected server advertises the
//! tool, and reports clearly when a Studio build does not.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Result, TerError};
use crate::mcp::protocol::ContentBlock;
use crate::mcp::{CallToolResult, McpManager};

pub const PLAY_TOOL: &str = "start_stop_play";
pub const STATE_TOOL: &str = "get_studio_state";
pub const CONSOLE_TOOL: &str = "get_console_output";
pub const CAPTURE_TOOL: &str = "screen_capture";

const MAX_TEXT: usize = 8000;

/// Playtest modes accepted by `start_stop_play`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayMode {
    StartPlay,
    RunServer,
    Stop,
}

impl PlayMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "start" | "start_play" | "play" | "test" => Some(Self::StartPlay),
            "server" | "run_server" => Some(Self::RunServer),
            "stop" | "reset" => Some(Self::Stop),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartPlay => "start_play",
            Self::RunServer => "run_server",
            Self::Stop => "stop",
        }
    }

    /// Arguments for `start_stop_play`; Studio's schema requires `mode`.
    pub fn arguments(self) -> Value {
        json!({ "mode": self.as_str() })
    }
}

/// An image returned by a tool, kept as base64 so it can be shown or saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Screenshot {
    pub mime: String,
    pub data: String,
}

/// Pulls the first image block out of a tool result.
pub fn extract_image(result: &CallToolResult) -> Option<Screenshot> {
    result.content.iter().find_map(|block| match block {
        ContentBlock::Image { data, mime_type } if !data.is_empty() => Some(Screenshot {
            mime: if mime_type.is_empty() {
                "image/png".to_string()
            } else {
                mime_type.clone()
            },
            data: data.clone(),
        }),
        _ => None,
    })
}

/// Decodes standard base64 (with or without padding, optionally a data URL).
pub fn decode_base64(input: &str) -> Option<Vec<u8>> {
    fn value(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some((byte - b'A') as u32),
            b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
            b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }

    let payload = match input.find(";base64,") {
        Some(index) => &input[index + ";base64,".len()..],
        None => input,
    };
    let bytes: Vec<u8> = payload
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut buffer = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            buffer |= value(*byte)? << (18 - 6 * index);
        }
        out.push((buffer >> 16) as u8);
        if chunk.len() > 2 {
            out.push((buffer >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(buffer as u8);
        }
    }
    Some(out)
}

fn text_of(result: &CallToolResult) -> Option<String> {
    let text = result.to_model_text(MAX_TEXT);
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

impl McpManager {
    /// Whether the connected server advertises `name`.
    pub async fn has_tool(&self, name: &str) -> bool {
        self.tools().await.iter().any(|tool| tool.name == name)
    }

    async fn require_tool(&self, name: &str, feature: &str) -> Result<()> {
        if self.has_tool(name).await {
            return Ok(());
        }
        Err(TerError::mcp(format!(
            "{feature} needs the `{name}` tool, which this session does not expose. \
             Connect Studio, update it, and make sure the tool is enabled under \
             Assistant → Manage MCP Servers, then reconnect."
        )))
    }

    async fn run_text_tool(&self, name: &str, arguments: Value, feature: &str) -> Result<String> {
        self.require_tool(name, feature).await?;
        let result = self.call_tool(name, arguments).await?;
        if result.is_error {
            return Err(TerError::mcp(
                text_of(&result).unwrap_or_else(|| format!("{name} failed in Studio")),
            ));
        }
        Ok(text_of(&result).unwrap_or_else(|| format!("{name} finished")))
    }

    /// Starts, stops or runs the server for the open place.
    pub async fn play(&self, mode: PlayMode) -> Result<String> {
        self.run_text_tool(PLAY_TOOL, mode.arguments(), "playtest control")
            .await
    }

    /// Current Studio mode and available data models.
    pub async fn studio_state(&self) -> Result<String> {
        self.run_text_tool(STATE_TOOL, json!({}), "reading the play state")
            .await
    }

    /// Playtest output log.
    pub async fn console_output(&self) -> Result<String> {
        self.run_text_tool(CONSOLE_TOOL, json!({}), "reading the Studio output")
            .await
    }

    /// Captures the Studio viewport.
    pub async fn screenshot(&self) -> Result<Screenshot> {
        self.require_tool(CAPTURE_TOOL, "viewport screenshots").await?;
        let result = self.call_tool(CAPTURE_TOOL, json!({})).await?;
        if result.is_error {
            return Err(TerError::mcp(
                text_of(&result).unwrap_or_else(|| "screen_capture failed in Studio".into()),
            ));
        }
        extract_image(&result)
            .ok_or_else(|| TerError::mcp("screen_capture returned no image data".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_with(blocks: Vec<ContentBlock>) -> CallToolResult {
        CallToolResult {
            content: blocks,
            is_error: false,
            structured: None,
        }
    }

    #[test]
    fn play_modes_match_documented_values() {
        assert_eq!(PlayMode::parse("start_play"), Some(PlayMode::StartPlay));
        assert_eq!(PlayMode::parse("PLAY"), Some(PlayMode::StartPlay));
        assert_eq!(PlayMode::parse("run_server"), Some(PlayMode::RunServer));
        assert_eq!(PlayMode::parse("stop"), Some(PlayMode::Stop));
        assert_eq!(PlayMode::parse("nonsense"), None);
        assert_eq!(
            PlayMode::StartPlay.arguments(),
            json!({ "mode": "start_play" })
        );
        assert_eq!(PlayMode::Stop.arguments(), json!({ "mode": "stop" }));
    }

    #[test]
    fn extracts_image_blocks_only() {
        let result = result_with(vec![
            ContentBlock::Text {
                text: "captured".into(),
            },
            ContentBlock::Image {
                data: "aGk=".into(),
                mime_type: "image/png".into(),
            },
        ]);
        let image = extract_image(&result).expect("image");
        assert_eq!(image.mime, "image/png");
        assert_eq!(image.data, "aGk=");

        assert!(extract_image(&result_with(vec![ContentBlock::Text {
            text: "text only".into()
        }]))
        .is_none());
        assert!(extract_image(&result_with(vec![ContentBlock::Image {
            data: String::new(),
            mime_type: "image/png".into()
        }]))
        .is_none());
    }

    #[test]
    fn base64_round_trips_png_header() {
        // "PNG" prefix used by every PNG file.
        let bytes = decode_base64("iVBORw0KGgo=").expect("decoded");
        assert_eq!(&bytes[..4], &[0x89, b'P', b'N', b'G']);

        let without_padding = decode_base64("iVBORw0KGgo").expect("decoded");
        assert_eq!(bytes, without_padding);

        let data_url = decode_base64("data:image/png;base64,iVBORw0KGgo=").expect("decoded");
        assert_eq!(bytes, data_url);

        assert!(decode_base64("!!!!").is_none());
        assert!(decode_base64("iVBO R").is_none());
    }
}
