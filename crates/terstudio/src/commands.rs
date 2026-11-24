//! Tauri command surface: the only bridge between the UI and `tercore`.

use crate::state::AppState;
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::oneshot;

use tercore::agent::{
    ActiveFileContext, Agent, AgentConfig, ConfirmationGate, ConfirmationRequest, ProjectContext,
};
use tercore::ai::types::Usage;
use tercore::ai::{ClientOptions, ModelInfo, TerRouterClient, ValidationReport};
use tercore::logging::{LogEntry, LogSource};
use tercore::luau::{self, Diagnostic};
use tercore::mcp::{CallToolResult, Diagnostics, DiscoveryReport, McpStatus, ToolSummary};
use tercore::project::{BackupInfo, FileNode, Project, SearchHit, VersionEntry};
use tercore::roblox::{self, PlaceSummary, RojoStatus};
use tercore::settings::Settings;
use tercore::{ErrorPayload, TerError};

type CmdResult<T> = Result<T, ErrorPayload>;

fn fail(code: &'static str, message: impl Into<String>) -> ErrorPayload {
    ErrorPayload {
        code,
        message: message.into(),
        retryable: false,
    }
}

fn to_payload(e: TerError) -> ErrorPayload {
    ErrorPayload::from(e)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub file_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub version: &'static str,
    pub settings: Settings,
    pub api_key_masked: Option<String>,
    pub secret_backend: String,
    pub projects: Vec<Project>,
    pub templates: Vec<TemplateInfo>,
    pub mcp_status: McpStatus,
    pub mcp_tools: Vec<ToolSummary>,
    pub models: Vec<ModelInfo>,
    pub usage: Usage,
    pub history: Vec<tercore::ai::types::ChatMessage>,
    pub logs: Vec<LogEntry>,
    pub prompt_history: Vec<crate::state::PromptHistoryEntry>,
    pub studio: DiscoveryReport,
    pub rojo: RojoStatus,
    pub data_dir: String,
}

#[tauri::command]
pub async fn bootstrap(state: State<'_, Arc<AppState>>) -> CmdResult<Bootstrap> {
    let settings = state.settings_snapshot().await;
    let api_key_masked = state.api_keys.masked().ok().flatten();
    let studio = tercore::mcp::discovery::discover(
        Some(settings.mcp.command.as_str()).filter(|c| !c.trim().is_empty()),
        &settings.mcp.args,
        if settings.mcp.transport == tercore::settings::McpTransportKind::Http {
            Some(settings.mcp.http_url.as_str())
        } else {
            None
        },
    );
    Ok(Bootstrap {
        version: tercore::APP_VERSION,
        settings,
        api_key_masked,
        secret_backend: state.api_keys.backend().to_string(),
        projects: state.projects.list(),
        templates: tercore::project::templates::all()
            .into_iter()
            .map(|t| TemplateInfo {
                id: t.id,
                name: t.name,
                description: t.description,
                file_count: t.files.len(),
            })
            .collect(),
        mcp_status: state.mcp.status().await,
        mcp_tools: state.mcp.tool_summaries().await,
        models: state.models.read().await.clone(),
        usage: state.usage.lock().await.clone(),
        history: state.history.lock().await.clone(),
        logs: state.log.snapshot(300),
        prompt_history: state.prompt_history.lock().await.clone(),
        studio,
        rojo: roblox::rojo_status(),
        data_dir: state.data_dir.display().to_string(),
    })
}

#[tauri::command]
pub async fn update_settings(
    state: State<'_, Arc<AppState>>,
    settings: Settings,
) -> CmdResult<Settings> {
    state.save_settings(settings).await.map_err(|e| fail("settings", e))
}

#[tauri::command]
pub async fn set_api_key(state: State<'_, Arc<AppState>>, key: String) -> CmdResult<Option<String>> {
    state.api_keys.save(&key).map_err(to_payload)?;
    state
        .log
        .info(LogSource::App, "Ter Router API key stored securely");
    state.api_keys.masked().map_err(to_payload)
}

#[tauri::command]
pub async fn clear_api_key(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.api_keys.clear().map_err(to_payload)?;
    *state.models.write().await = Vec::new();
    state.log.info(LogSource::App, "Ter Router API key removed");
    Ok(())
}

async fn client(state: &AppState) -> CmdResult<TerRouterClient> {
    let key = state
        .api_keys
        .current()
        .map_err(to_payload)?
        .ok_or_else(|| to_payload(TerError::MissingApiKey))?;
    let settings = state.settings_snapshot().await;
    TerRouterClient::new(ClientOptions {
        base_url: settings.ai.base_url.clone(),
        api_key: key,
        request_timeout_secs: settings.ai.request_timeout_secs,
        max_retries: settings.ai.max_retries,
    })
    .map_err(to_payload)
}

#[tauri::command]
pub async fn validate_api_key(state: State<'_, Arc<AppState>>) -> CmdResult<ValidationReport> {
    let client = client(&state).await?;
    let report = client.validate().await.map_err(to_payload)?;
    state.log.info(
        LogSource::Ai,
        format!(
            "Ter Router reachable: {} models in {} ms",
            report.model_count, report.latency_ms
        ),
    );
    Ok(report)
}

#[tauri::command]
pub async fn list_models(
    state: State<'_, Arc<AppState>>,
    refresh: Option<bool>,
) -> CmdResult<Vec<ModelInfo>> {
    if !refresh.unwrap_or(false) {
        let cached = state.models.read().await.clone();
        if !cached.is_empty() {
            return Ok(cached);
        }
    }
    let client = client(&state).await?;
    let models = client.list_models().await.map_err(to_payload)?;
    state.log.info(
        LogSource::Ai,
        format!("discovered {} Ter Router models", models.len()),
    );
    *state.models.write().await = models.clone();
    Ok(models)
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn list_projects(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<Project>> {
    Ok(state.projects.list())
}

#[tauri::command]
pub async fn create_project(
    state: State<'_, Arc<AppState>>,
    name: String,
    template: String,
    description: Option<String>,
) -> CmdResult<Project> {
    let project = state
        .projects
        .create(&name, &template, description.as_deref().unwrap_or(""))
        .map_err(to_payload)?;
    state.log.info(
        LogSource::Project,
        format!("created project `{}` ({})", project.name, project.template),
    );
    let mut settings = state.settings_snapshot().await;
    settings.ui.last_project = Some(project.id.clone());
    let _ = state.save_settings(settings).await;
    Ok(project)
}

#[tauri::command]
pub async fn import_project(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> CmdResult<Project> {
    let project = state
        .projects
        .import(std::path::Path::new(&path), None)
        .map_err(to_payload)?;
    state.log.info(
        LogSource::Project,
        format!("imported {} from {}", project.name, path),
    );
    Ok(project)
}

#[tauri::command]
pub async fn remove_project(
    state: State<'_, Arc<AppState>>,
    id: String,
    delete_files: Option<bool>,
) -> CmdResult<()> {
    state
        .projects
        .remove(&id, delete_files.unwrap_or(false))
        .map_err(to_payload)
}

#[tauri::command]
pub async fn rename_project(
    state: State<'_, Arc<AppState>>,
    id: String,
    name: String,
) -> CmdResult<Project> {
    state.projects.rename(&id, &name).map_err(to_payload)
}

#[tauri::command]
pub async fn project_tree(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<Vec<FileNode>> {
    state.projects.tree(&id).map_err(to_payload)
}

#[tauri::command]
pub async fn read_file(
    state: State<'_, Arc<AppState>>,
    id: String,
    path: String,
) -> CmdResult<String> {
    state.projects.read_file(&id, &path).map_err(to_payload)
}

#[tauri::command]
pub async fn write_file(
    state: State<'_, Arc<AppState>>,
    id: String,
    path: String,
    contents: String,
) -> CmdResult<()> {
    state
        .projects
        .write_file(&id, &path, &contents)
        .map_err(to_payload)
}

#[tauri::command]
pub async fn create_file(
    state: State<'_, Arc<AppState>>,
    id: String,
    path: String,
    contents: Option<String>,
) -> CmdResult<()> {
    let seed = contents.unwrap_or_else(|| default_file_contents(&path));
    state
        .projects
        .create_file(&id, &path, &seed)
        .map_err(to_payload)
}

fn default_file_contents(path: &str) -> String {
    if path.ends_with(".luau") || path.ends_with(".lua") {
        "--!strict\n\n".to_string()
    } else if path.ends_with(".json") {
        "{\n  \n}\n".to_string()
    } else {
        String::new()
    }
}

#[tauri::command]
pub async fn delete_path(
    state: State<'_, Arc<AppState>>,
    id: String,
    path: String,
) -> CmdResult<()> {
    state.projects.delete_path(&id, &path).map_err(to_payload)
}

#[tauri::command]
pub async fn rename_path(
    state: State<'_, Arc<AppState>>,
    id: String,
    from: String,
    to: String,
) -> CmdResult<()> {
    state
        .projects
        .rename_path(&id, &from, &to)
        .map_err(to_payload)
}

#[tauri::command]
pub async fn search_project(
    state: State<'_, Arc<AppState>>,
    id: String,
    query: String,
) -> CmdResult<Vec<SearchHit>> {
    state.projects.search(&id, &query, 200).map_err(to_payload)
}

#[tauri::command]
pub async fn analyze_file(path: String, contents: String) -> CmdResult<Vec<Diagnostic>> {
    Ok(luau::analyze(&contents, &path))
}

#[tauri::command]
pub async fn backup_project(
    state: State<'_, Arc<AppState>>,
    id: String,
    label: Option<String>,
) -> CmdResult<BackupInfo> {
    let info = state
        .projects
        .backup(&id, label.as_deref().unwrap_or("manual"))
        .map_err(to_payload)?;
    state
        .log
        .info(LogSource::Project, format!("backup created ({} bytes)", info.bytes));
    Ok(info)
}

#[tauri::command]
pub async fn list_backups(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> CmdResult<Vec<BackupInfo>> {
    state.projects.backups(&id).map_err(to_payload)
}

#[tauri::command]
pub async fn restore_backup(
    state: State<'_, Arc<AppState>>,
    id: String,
    backup_id: String,
) -> CmdResult<usize> {
    state
        .projects
        .restore_backup(&id, &backup_id)
        .map_err(to_payload)
}

#[tauri::command]
pub async fn list_versions(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> CmdResult<Vec<VersionEntry>> {
    state.projects.versions(&id).map_err(to_payload)
}

#[tauri::command]
pub async fn restore_version(
    state: State<'_, Arc<AppState>>,
    id: String,
    version_id: String,
) -> CmdResult<String> {
    state
        .projects
        .restore_version(&id, &version_id)
        .map_err(to_payload)
}

// ---------------------------------------------------------------------------
// MCP / Studio
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn mcp_status(state: State<'_, Arc<AppState>>) -> CmdResult<McpStatus> {
    Ok(state.mcp.status().await)
}

#[tauri::command]
pub async fn mcp_tools(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<ToolSummary>> {
    Ok(state.mcp.tool_summaries().await)
}

#[tauri::command]
pub async fn mcp_reconnect(state: State<'_, Arc<AppState>>) -> CmdResult<McpStatus> {
    state.mcp.reconnect().await.map_err(to_payload)?;
    Ok(state.mcp.status().await)
}

#[tauri::command]
pub async fn mcp_disconnect(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.mcp.disconnect().await;
    Ok(())
}

#[tauri::command]
pub async fn mcp_diagnostics(state: State<'_, Arc<AppState>>) -> CmdResult<Diagnostics> {
    Ok(state.mcp.diagnostics().await)
}

#[tauri::command]
pub async fn mcp_call_tool(
    state: State<'_, Arc<AppState>>,
    name: String,
    arguments: Option<serde_json::Value>,
) -> CmdResult<CallToolResult> {
    state
        .mcp
        .call_tool(
            &name,
            arguments.unwrap_or_else(|| serde_json::json!({})),
        )
        .await
        .map_err(to_payload)
}

#[tauri::command]
pub async fn mcp_refresh_tools(state: State<'_, Arc<AppState>>) -> CmdResult<Vec<ToolSummary>> {
    state.mcp.refresh_tools().await.map_err(to_payload)
}

// --- Studio testing: playtest, console and viewport capture -------------------

#[tauri::command]
pub async fn studio_play(state: State<'_, Arc<AppState>>, mode: String) -> CmdResult<String> {
    let mode = tercore::mcp::studio::PlayMode::parse(&mode)
        .ok_or_else(|| fail("invalid_mode", "mode must be start_play, run_server or stop"))?;
    let text = state.mcp.play(mode).await.map_err(to_payload)?;
    state
        .log
        .info(LogSource::Mcp, format!("Studio play mode → {}", mode.as_str()));
    Ok(text)
}

#[tauri::command]
pub async fn studio_state(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    state.mcp.studio_state().await.map_err(to_payload)
}

#[tauri::command]
pub async fn studio_console(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    state.mcp.console_output().await.map_err(to_payload)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotSaved {
    pub path: String,
    pub data_url: String,
    pub bytes: usize,
    pub mime: String,
}

#[tauri::command]
pub async fn studio_screenshot(state: State<'_, Arc<AppState>>) -> CmdResult<ScreenshotSaved> {
    let shot = state.mcp.screenshot().await.map_err(to_payload)?;
    let bytes = tercore::mcp::studio::decode_base64(&shot.data)
        .ok_or_else(|| fail("bad_response", "Studio returned image data that is not valid base64"))?;
    let dir = state.data_dir.join("screenshots");
    std::fs::create_dir_all(&dir)
        .map_err(|e| to_payload(TerError::io(dir.display().to_string(), e)))?;
    let ext = if shot.mime.contains("jpeg") { "jpg" } else { "png" };
    let path = dir.join(format!(
        "studio-{}.{ext}",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    std::fs::write(&path, &bytes)
        .map_err(|e| to_payload(TerError::io(path.display().to_string(), e)))?;
    state.log.info(
        LogSource::Mcp,
        format!("Captured Studio screenshot ({} KB)", bytes.len() / 1024),
    );
    Ok(ScreenshotSaved {
        path: path.display().to_string(),
        data_url: format!("data:{};base64,{}", shot.mime, shot.data),
        bytes: bytes.len(),
        mime: shot.mime,
    })
}

#[tauri::command]
pub async fn studio_discover(state: State<'_, Arc<AppState>>) -> CmdResult<DiscoveryReport> {
    let settings = state.settings_snapshot().await;
    Ok(tercore::mcp::discovery::discover(
        Some(settings.mcp.command.as_str()).filter(|c| !c.trim().is_empty()),
        &settings.mcp.args,
        if settings.mcp.transport == tercore::settings::McpTransportKind::Http {
            Some(settings.mcp.http_url.as_str())
        } else {
            None
        },
    ))
}

#[tauri::command]
pub async fn studio_launch(state: State<'_, Arc<AppState>>) -> CmdResult<String> {
    let report = tercore::mcp::discovery::discover(None, &[], None);
    let exe = report.studio_exe.ok_or_else(|| {
        to_payload(TerError::McpNotFound(
            "Roblox Studio was not found. Install it from https://create.roblox.com/".into(),
        ))
    })?;
    tercore::mcp::discovery::launch_studio(&exe)
        .map_err(|e| fail("studio", format!("could not start Roblox Studio: {e}")))?;
    state.log.info(
        LogSource::Studio,
        format!("launched Roblox Studio ({})", exe.display()),
    );
    Ok(exe.display().to_string())
}

#[tauri::command]
pub async fn rojo_status() -> CmdResult<RojoStatus> {
    Ok(roblox::rojo_status())
}

#[tauri::command]
pub async fn rojo_build(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> CmdResult<String> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| to_payload(TerError::ProjectNotFound(id.clone())))?;
    let output = project.path.join("build").join(format!("{}.rbxlx", project.name));
    let path = roblox::rojo_build(&project.path, &output).map_err(to_payload)?;
    state
        .log
        .info(LogSource::Project, format!("rojo build → {}", path.display()));
    Ok(path.display().to_string())
}

#[tauri::command]
pub async fn export_model(state: State<'_, Arc<AppState>>, id: String) -> CmdResult<String> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| to_payload(TerError::ProjectNotFound(id.clone())))?;
    let files = roblox::collect_luau_files(&project.path).map_err(to_payload)?;
    if files.is_empty() {
        return Err(fail(
            "empty_project",
            "This project has no Luau files to export yet.",
        ));
    }
    let output = project
        .path
        .join("build")
        .join(format!("{}.rbxmx", project.name));
    let path = roblox::export_model_file(&project.path, &files, &project.name, &output)
        .map_err(to_payload)?;
    state.log.info(
        LogSource::Project,
        format!("exported {} scripts → {}", files.len(), path.display()),
    );
    Ok(path.display().to_string())
}

#[tauri::command]
pub async fn summarize_place(path: String) -> CmdResult<PlaceSummary> {
    roblox::summarize_place(std::path::Path::new(&path)).map_err(to_payload)
}

#[tauri::command]
pub async fn ensure_rojo_project(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> CmdResult<String> {
    let project = state
        .projects
        .get(&id)
        .ok_or_else(|| to_payload(TerError::ProjectNotFound(id.clone())))?;
    let path =
        roblox::ensure_rojo_project(&project.path, &project.name).map_err(to_payload)?;
    Ok(path.display().to_string())
}

// ---------------------------------------------------------------------------
// AI assistant
// ---------------------------------------------------------------------------

struct UiGate {
    app: AppHandle,
    pending: Arc<tokio::sync::Mutex<std::collections::HashMap<String, oneshot::Sender<bool>>>>,
}

#[async_trait::async_trait]
impl ConfirmationGate for UiGate {
    async fn confirm(&self, request: &ConfirmationRequest) -> bool {
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request.id.clone(), tx);
        let _ = self.app.emit("tercode://confirm", request);
        match tokio::time::timeout(Duration::from_secs(900), rx).await {
            Ok(Ok(approved)) => approved,
            _ => {
                self.pending.lock().await.remove(&request.id);
                false
            }
        }
    }
}

fn flatten_files(nodes: &[FileNode], out: &mut Vec<String>) {
    for node in nodes {
        if node.is_dir {
            if let Some(children) = &node.children {
                flatten_files(children, out);
            }
        } else {
            out.push(node.path.clone());
        }
    }
}

#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    text: String,
    project_id: Option<String>,
    active_path: Option<String>,
) -> CmdResult<()> {
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err(fail("empty_prompt", "Type what you want to build."));
    }
    if state
        .agent_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err(fail(
            "busy",
            "The assistant is still working on the previous request.",
        ));
    }

    let state = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        let result = run_agent(&app, state.clone(), trimmed, project_id, active_path).await;
        if let Err(e) = result {
            let _ = app.emit("tercode://agent-error", &e);
        }
        state.agent_running.store(false, Ordering::SeqCst);
    });
    Ok(())
}

async fn run_agent(
    app: &AppHandle,
    state: Arc<AppState>,
    text: String,
    project_id: Option<String>,
    active_path: Option<String>,
) -> CmdResult<()> {
    let key = match state.api_keys.current() {
        Ok(Some(key)) => key,
        _ => {
            return Err(fail(
                "missing_api_key",
                "Add your Ter Router API key in Settings → AI Provider.",
            ))
        }
    };
    let settings = state.settings_snapshot().await;
    if settings.ai.model.trim().is_empty() {
        return Err(fail(
            "no_model",
            "Choose a model in the top bar (Refresh models if the list is empty).",
        ));
    }

    state.record_prompt(&text).await;

    let client = TerRouterClient::new(ClientOptions {
        base_url: settings.ai.base_url.clone(),
        api_key: key,
        request_timeout_secs: settings.ai.request_timeout_secs,
        max_retries: settings.ai.max_retries,
    })
    .map_err(to_payload)?;

    // Build live project context for the prompt.
    let mut context = ProjectContext {
        studio_status: state.mcp.status().await.label().to_string(),
        templates: tercore::project::templates::all()
            .into_iter()
            .map(|t| t.id.to_string())
            .collect(),
        ..Default::default()
    };
    if let Some(id) = &project_id {
        if let Some(project) = state.projects.get(id) {
            context.project_name = Some(project.name.clone());
            context.project_path = Some(project.path.display().to_string());
            if let Ok(tree) = state.projects.tree(id) {
                flatten_files(&tree, &mut context.files);
                context.files.truncate(300);
            }
        }
        if let Some(path) = &active_path {
            if let Ok(contents) = state.projects.read_file(id, path) {
                let diagnostics = if path.ends_with(".luau") || path.ends_with(".lua") {
                    luau::analyze(&contents, path)
                } else {
                    Vec::new()
                };
                context.active_file = Some(ActiveFileContext {
                    path: path.clone(),
                    contents,
                    diagnostics,
                });
            }
        }
    }

    let config = AgentConfig {
        model: settings.ai.model.clone(),
        temperature: settings.ai.temperature,
        max_tokens: settings.ai.max_tokens,
        max_tool_rounds: 12,
        require_confirmation: settings.mcp.confirm_destructive,
        custom_instructions: settings.ai.custom_instructions.clone(),
    };

    let agent = Agent::new(
        Arc::new(client),
        state.mcp.clone(),
        state.log.clone(),
        config,
    );
    let mut rx = agent.subscribe();
    let forward_app = app.clone();
    let forwarder = tauri::async_runtime::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let _ = forward_app.emit("tercode://agent", &event);
        }
    });

    let gate: Arc<dyn ConfirmationGate> = Arc::new(UiGate {
        app: app.clone(),
        pending: state.confirmations.clone(),
    });

    let mut history = state.history.lock().await.clone();
    let outcome = agent.run(&mut history, &text, &context, gate).await;
    *state.history.lock().await = history;
    forwarder.abort();

    match outcome {
        Ok(outcome) => {
            {
                let mut usage = state.usage.lock().await;
                usage.accumulate(&outcome.usage);
            }
            let _ = app.emit("tercode://agent-done", &outcome);
            Ok(())
        }
        Err(e) => Err(to_payload(e)),
    }
}

#[tauri::command]
pub async fn confirm_tool(
    state: State<'_, Arc<AppState>>,
    id: String,
    approved: bool,
) -> CmdResult<()> {
    if let Some(sender) = state.confirmations.lock().await.remove(&id) {
        let _ = sender.send(approved);
        state.log.info(
            LogSource::Ai,
            format!("tool call {id} {}", if approved { "approved" } else { "declined" }),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn clear_conversation(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.clear_conversation().await;
    Ok(())
}

#[tauri::command]
pub async fn prompt_history(
    state: State<'_, Arc<AppState>>,
) -> CmdResult<Vec<crate::state::PromptHistoryEntry>> {
    Ok(state.prompt_history.lock().await.clone())
}

// ---------------------------------------------------------------------------
// Logs, links, updates
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn get_logs(
    state: State<'_, Arc<AppState>>,
    limit: Option<usize>,
) -> CmdResult<Vec<LogEntry>> {
    Ok(state.log.snapshot(limit.unwrap_or(400)))
}

#[tauri::command]
pub async fn clear_logs(state: State<'_, Arc<AppState>>) -> CmdResult<()> {
    state.log.clear();
    Ok(())
}

#[tauri::command]
pub async fn open_external(url: String) -> CmdResult<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(fail("invalid_url", "Only http(s) links can be opened."));
    }
    #[cfg(windows)]
    let result = std::process::Command::new("cmd")
        .args(["/c", "start", "", &url])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(&url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(&url).spawn();

    result
        .map(|_| ())
        .map_err(|e| fail("open_failed", format!("could not open the browser: {e}")))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheck {
    pub current: String,
    pub latest: Option<String>,
    pub available: bool,
    pub url: String,
    pub notes: Option<String>,
}

#[tauri::command]
pub async fn check_updates() -> CmdResult<UpdateCheck> {
    let client = reqwest::Client::builder()
        .user_agent(tercore::USER_AGENT)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| fail("network", format!("could not build HTTP client: {e}")))?;
    let response = client
        .get("https://api.github.com/repos/itriedcoding/TerCode/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| fail("network", format!("could not reach GitHub: {e}")))?;
    if !response.status().is_success() {
        return Err(fail(
            "no_releases",
            format!(
                "No published release found yet (GitHub responded {}).",
                response.status()
            ),
        ));
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| fail("bad_response", format!("unexpected GitHub response: {e}")))?;
    let latest = body
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim_start_matches('v')
        .to_string();
    let url = body
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("https://github.com/itriedcoding/TerCode/releases")
        .to_string();
    let notes = body
        .get("body")
        .and_then(|v| v.as_str())
        .map(|s| s.chars().take(1500).collect::<String>());
    Ok(UpdateCheck {
        current: tercore::APP_VERSION.to_string(),
        available: semver_newer(&latest, tercore::APP_VERSION),
        latest: if latest.is_empty() { None } else { Some(latest) },
        url,
        notes,
    })
}

fn semver_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split(['.', '-', '+'])
            .map(|p| {
                p.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect()
    }
    let a = parts(candidate);
    let b = parts(current);
    for i in 0..3 {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        if av != bv {
            return av > bv;
        }
    }
    false
}

#[tauri::command]
pub async fn app_info() -> CmdResult<serde_json::Value> {
    Ok(serde_json::json!({
        "version": tercore::APP_VERSION,
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "dataDir": tercore::settings::data_dir().display().to_string(),
    }))
}

#[tauri::command]
pub async fn pick_folder(app: AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = oneshot::channel();
    app.dialog().file().pick_folder(move |folder| {
        let _ = tx.send(folder.and_then(|f| f.into_path().ok()));
    });
    let picked = rx.await.map_err(|_| fail("dialog", "folder picker closed"))?;
    Ok(picked.map(|p| p.display().to_string()))
}

#[tauri::command]
pub async fn pick_file(app: AppHandle) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .add_filter("Roblox place or model", &["rbxlx", "rbxmx", "rbxl", "rbxm"])
        .pick_file(move |file| {
            let _ = tx.send(file.and_then(|f| f.into_path().ok()));
        });
    let picked = rx.await.map_err(|_| fail("dialog", "file picker closed"))?;
    Ok(picked.map(|p| p.display().to_string()))
}
