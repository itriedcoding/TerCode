//! Process-wide state shared by every Tauri command.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex, RwLock};

use tercore::ai::types::{ChatMessage, ModelInfo, Usage};
use tercore::logging::ActivityLog;
use tercore::mcp::McpManager;
use tercore::project::ProjectStore;
use tercore::secrets::{default_store, ApiKeyManager};
use tercore::settings::{data_dir, settings_path, Settings};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptHistoryEntry {
    pub text: String,
    pub at: DateTime<Utc>,
}

pub struct AppState {
    pub settings: RwLock<Settings>,
    pub settings_path: PathBuf,
    pub data_dir: PathBuf,
    pub log: ActivityLog,
    pub projects: ProjectStore,
    pub api_keys: ApiKeyManager,
    pub mcp: Arc<McpManager>,
    pub history: Mutex<Vec<ChatMessage>>,
    pub usage: Mutex<Usage>,
    pub models: RwLock<Vec<ModelInfo>>,
    /// Pending destructive-tool approvals, keyed by tool-call id.
    pub confirmations: Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>,
    pub agent_running: AtomicBool,
    pub prompt_history: Mutex<Vec<PromptHistoryEntry>>,
}

impl AppState {
    pub fn initialize() -> Result<Self, String> {
        let data_dir = data_dir();
        std::fs::create_dir_all(&data_dir).map_err(|e| {
            format!(
                "could not create the TerCode data folder at {}: {e}",
                data_dir.display()
            )
        })?;

        let log = ActivityLog::new();
        let path = settings_path();
        let (settings, warning) = Settings::load_or_default(&path);
        if let Some(warning) = warning {
            log.warn(tercore::LogSource::App, warning);
        }

        let projects = ProjectStore::open(data_dir.join("projects"))
            .map_err(|e| format!("could not open the project workspace: {e}"))?;
        let api_keys = ApiKeyManager::new(default_store(&data_dir));
        let mcp = McpManager::new(log.clone(), settings.mcp.clone());

        log.info(
            tercore::LogSource::App,
            format!(
                "TerCode {} starting (settings: {}, secrets: {})",
                tercore::APP_VERSION,
                path.display(),
                api_keys.backend()
            ),
        );

        let prompt_history = load_prompt_history(&data_dir);

        Ok(Self {
            settings: RwLock::new(settings),
            settings_path: path,
            data_dir,
            log,
            projects,
            api_keys,
            mcp,
            history: Mutex::new(Vec::new()),
            usage: Mutex::new(Usage::default()),
            models: RwLock::new(Vec::new()),
            confirmations: Arc::new(Mutex::new(HashMap::new())),
            agent_running: AtomicBool::new(false),
            prompt_history: Mutex::new(prompt_history),
        })
    }

    pub async fn settings_snapshot(&self) -> Settings {
        self.settings.read().await.clone()
    }

    pub async fn save_settings(&self, settings: Settings) -> Result<Settings, String> {
        let mut normalized = settings;
        normalized.normalize();
        normalized
            .save(&self.settings_path)
            .map_err(|e| format!("could not save settings: {e}"))?;
        *self.settings.write().await = normalized.clone();
        self.mcp.update_settings(normalized.mcp.clone()).await;
        Ok(normalized)
    }

    pub async fn record_prompt(&self, text: &str) {
        let mut history = self.prompt_history.lock().await;
        history.insert(
            0,
            PromptHistoryEntry {
                text: text.to_string(),
                at: Utc::now(),
            },
        );
        history.truncate(100);
        let json = match serde_json::to_string_pretty(&*history) {
            Ok(json) => json,
            Err(_) => return,
        };
        let path = self.data_dir.join("prompt-history.json");
        let _ = std::fs::write(path, json);
    }

    pub async fn clear_conversation(&self) {
        self.history.lock().await.clear();
        *self.usage.lock().await = Usage::default();
    }
}

fn load_prompt_history(data_dir: &std::path::Path) -> Vec<PromptHistoryEntry> {
    let path = data_dir.join("prompt-history.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<PromptHistoryEntry>>(&raw).ok())
        .unwrap_or_default()
}
