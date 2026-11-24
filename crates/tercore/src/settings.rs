//! Persisted application settings (`settings.json`), never secrets.

use crate::error::{Result, TerError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_BASE_URL: &str = "https://api.terrouter.com/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiSettings {
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub request_timeout_secs: u64,
    pub max_retries: u32,
    /// Extra system instructions appended to the built-in Roblox prompt.
    pub custom_instructions: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: String::new(),
            temperature: 0.3,
            max_tokens: 8192,
            request_timeout_secs: 180,
            max_retries: 3,
            custom_instructions: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportKind {
    /// Spawn a local process and speak JSON-RPC over stdin/stdout (Roblox's
    /// built-in Studio MCP server uses this).
    #[default]
    Stdio,
    /// MCP Streamable HTTP endpoint (community servers, remote tunnels).
    Http,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpSettings {
    pub auto_connect: bool,
    pub auto_reconnect: bool,
    pub confirm_destructive: bool,
    pub transport: McpTransportKind,
    /// Custom command override. Empty means "auto-detect".
    pub command: String,
    pub args: Vec<String>,
    pub http_url: String,
    /// Empty means "first Studio reported by list_roblox_studios".
    pub studio_id: String,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            auto_connect: true,
            auto_reconnect: true,
            confirm_destructive: true,
            transport: McpTransportKind::Stdio,
            command: String::new(),
            args: Vec::new(),
            http_url: "http://127.0.0.1:44755/mcp".to_string(),
            studio_id: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EditorSettings {
    pub font_size: u32,
    pub tab_size: u32,
    pub word_wrap: bool,
    pub autosave_secs: u64,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: 13,
            tab_size: 4,
            word_wrap: false,
            autosave_secs: 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UiSettings {
    pub last_project: Option<String>,
    pub sidebar_width: u32,
    pub assistant_width: u32,
    pub bottom_panel_height: u32,
    pub show_bottom_panel: bool,
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            last_project: None,
            sidebar_width: 248,
            assistant_width: 380,
            bottom_panel_height: 208,
            show_bottom_panel: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub ai: AiSettings,
    #[serde(default)]
    pub mcp: McpSettings,
    #[serde(default)]
    pub editor: EditorSettings,
    #[serde(default)]
    pub ui: UiSettings,
}

impl Settings {
    pub fn load_or_default(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<Settings>(&raw) {
                Ok(mut s) => {
                    s.normalize();
                    (s, None)
                }
                Err(e) => (
                    Settings::default(),
                    Some(format!("settings.json was unreadable ({e}); defaults restored")),
                ),
            },
            Err(_) => (Settings::default(), None),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| TerError::io(parent.display().to_string(), e))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| TerError::json("settings.json", e))?;
        // Write to a temp file first so a crash cannot truncate settings.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| TerError::io(tmp.display().to_string(), e))?;
        std::fs::rename(&tmp, path).map_err(|e| TerError::io(path.display().to_string(), e))?;
        Ok(())
    }

    /// Clamp values that could break the app if hand-edited.
    pub fn normalize(&mut self) {
        if self.ai.base_url.trim().is_empty() {
            self.ai.base_url = DEFAULT_BASE_URL.to_string();
        }
        self.ai.base_url = self.ai.base_url.trim().trim_end_matches('/').to_string();
        if !self.ai.base_url.starts_with("http") {
            self.ai.base_url = DEFAULT_BASE_URL.to_string();
        }
        self.ai.temperature = self.ai.temperature.clamp(0.0, 2.0);
        self.ai.max_tokens = self.ai.max_tokens.clamp(256, 128_000);
        self.ai.request_timeout_secs = self.ai.request_timeout_secs.clamp(10, 900);
        self.ai.max_retries = self.ai.max_retries.min(8);
        self.editor.font_size = self.editor.font_size.clamp(9, 28);
        self.editor.tab_size = self.editor.tab_size.clamp(2, 8);
        self.editor.autosave_secs = self.editor.autosave_secs.min(600);
        self.ui.sidebar_width = self.ui.sidebar_width.clamp(180, 420);
        self.ui.assistant_width = self.ui.assistant_width.clamp(300, 640);
        self.ui.bottom_panel_height = self.ui.bottom_panel_height.clamp(120, 480);
        self.mcp.args.retain(|a| !a.is_empty());
    }
}

/// Directory holding settings, projects, logs and caches.
pub fn data_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("TERCODE_DATA_DIR") {
        if !custom.trim().is_empty() {
            return PathBuf::from(custom);
        }
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("TerCode")
}

pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_point_at_ter_router() {
        let s = Settings::default();
        assert_eq!(s.ai.base_url, "https://api.terrouter.com/v1");
        assert!(s.mcp.auto_connect);
        assert!(s.mcp.confirm_destructive);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut s = Settings::default();
        s.ai.model = "anthropic/claude-sonnet-4.5".into();
        s.mcp.studio_id = "studio-1".into();
        s.editor.font_size = 15;
        s.save(&path).unwrap();
        let (loaded, warning) = Settings::load_or_default(&path);
        assert!(warning.is_none());
        assert_eq!(loaded, s);
    }

    #[test]
    fn corrupt_file_falls_back_with_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{not json").unwrap();
        let (loaded, warning) = Settings::load_or_default(&path);
        assert_eq!(loaded, Settings::default());
        assert!(warning.unwrap().contains("unreadable"));
    }

    #[test]
    fn normalize_clamps_and_strips_trailing_slash() {
        let mut s = Settings::default();
        s.ai.base_url = "https://api.terrouter.com/v1/".into();
        s.ai.temperature = 9.0;
        s.ai.max_tokens = 1;
        s.ai.request_timeout_secs = 1;
        s.mcp.args = vec!["--stdio".into(), "".into()];
        s.normalize();
        assert_eq!(s.ai.base_url, "https://api.terrouter.com/v1");
        assert_eq!(s.ai.temperature, 2.0);
        assert_eq!(s.ai.max_tokens, 256);
        assert_eq!(s.ai.request_timeout_secs, 10);
        assert_eq!(s.mcp.args, vec!["--stdio".to_string()]);
    }

    #[test]
    fn missing_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let (loaded, warning) = Settings::load_or_default(&dir.path().join("nope.json"));
        assert!(warning.is_none());
        assert_eq!(loaded, Settings::default());
    }
}
