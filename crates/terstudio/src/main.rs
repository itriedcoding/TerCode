//! TerCode desktop entry point.
//!
//! The shell is intentionally thin: it owns the window, forwards backend events
//! to the UI and exposes `tercore` through Tauri commands.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod state;

use std::sync::Arc;
use tauri::{Emitter, Manager};

use state::AppState;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let state = AppState::initialize()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let state = Arc::new(state);
            app.manage(state.clone());

            // MCP: connect automatically at launch and keep reconnecting while
            // Studio starts, stops or restarts.
            let mcp = state.mcp.clone();
            tauri::async_runtime::spawn(async move {
                mcp.start_supervisor().await;
            });

            // Activity log → bottom panel.
            let log_handle = app.handle().clone();
            let mut log_rx = state.log.subscribe();
            tauri::async_runtime::spawn(async move {
                while let Ok(entry) = log_rx.recv().await {
                    let _ = log_handle.emit("tercode://log", &entry);
                }
            });

            // MCP status / tools / tool activity → connection indicator and tool explorer.
            let mcp_handle = app.handle().clone();
            let mut mcp_rx = state.mcp.subscribe();
            tauri::async_runtime::spawn(async move {
                while let Ok(event) = mcp_rx.recv().await {
                    let _ = mcp_handle.emit("tercode://mcp", &event);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::update_settings,
            commands::set_api_key,
            commands::clear_api_key,
            commands::validate_api_key,
            commands::list_models,
            commands::list_projects,
            commands::create_project,
            commands::import_project,
            commands::remove_project,
            commands::rename_project,
            commands::project_tree,
            commands::read_file,
            commands::write_file,
            commands::create_file,
            commands::delete_path,
            commands::rename_path,
            commands::search_project,
            commands::analyze_file,
            commands::analyze_project,
            commands::backup_project,
            commands::list_backups,
            commands::restore_backup,
            commands::list_versions,
            commands::restore_version,
            commands::mcp_status,
            commands::mcp_tools,
            commands::mcp_reconnect,
            commands::mcp_disconnect,
            commands::mcp_diagnostics,
            commands::mcp_call_tool,
            commands::mcp_refresh_tools,
            commands::studio_play,
            commands::studio_state,
            commands::studio_console,
            commands::studio_screenshot,
            commands::studio_discover,
            commands::studio_launch,
            commands::rojo_status,
            commands::rojo_build,
            commands::export_model,
            commands::summarize_place,
            commands::ensure_rojo_project,
            commands::send_message,
            commands::run_self_test,
            commands::confirm_tool,
            commands::clear_conversation,
            commands::prompt_history,
            commands::get_logs,
            commands::clear_logs,
            commands::open_external,
            commands::check_updates,
            commands::app_info,
            commands::pick_folder,
            commands::pick_file,
        ])
        .run(tauri::generate_context!())
        .expect("TerCode failed to start");
}
