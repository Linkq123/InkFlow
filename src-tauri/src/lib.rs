mod asset;
mod commands;
mod document;
mod encoding;
mod error;
mod export;
mod fileio;
pub mod model;
mod recovery;
mod settings;
mod workspace;

use std::sync::Arc;

use directories::ProjectDirs;
use parking_lot::Mutex;
use tauri::{Emitter, Manager};

use document::DocumentStore;
use recovery::RecoveryStore;
use settings::SettingsStore;
use workspace::WorkspaceStore;

pub struct AppState {
    documents: Arc<DocumentStore>,
    workspace: Arc<WorkspaceStore>,
    recovery: Arc<RecoveryStore>,
    settings: Arc<SettingsStore>,
    startup_paths: Mutex<Vec<String>>,
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            let paths: Vec<String> = args
                .into_iter()
                .skip(1)
                .filter(|value| !value.starts_with('-'))
                .collect();
            if !paths.is_empty() {
                let _ = app.emit("app-open-paths", paths);
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .setup(|app| {
            let project = ProjectDirs::from("com", "InkFlow", "InkFlow")
                .ok_or("Unable to resolve the InkFlow application data directory")?;
            let data = project.data_local_dir();
            std::fs::create_dir_all(data)?;
            let startup_paths = std::env::args()
                .skip(1)
                .filter(|value| !value.starts_with('-') && std::path::Path::new(value).exists())
                .collect();
            app.manage(AppState {
                documents: Arc::new(DocumentStore::new()),
                workspace: Arc::new(WorkspaceStore::new()),
                recovery: Arc::new(RecoveryStore::new(data.join("Recovery"))?),
                settings: Arc::new(SettingsStore::load(data.join("settings.json"))),
                startup_paths: Mutex::new(startup_paths),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_paths,
            commands::take_startup_paths,
            commands::reload_document,
            commands::close_document,
            commands::save_document,
            commands::save_document_as,
            commands::check_external_changes,
            commands::open_workspace,
            commands::refresh_workspace,
            commands::search_workspace,
            commands::create_workspace_entry,
            commands::rename_workspace_entry,
            commands::trash_workspace_entry,
            commands::write_asset,
            commands::load_resource,
            commands::checkpoint_document,
            commands::list_recovery,
            commands::restore_revision,
            commands::delete_recovery,
            commands::get_settings,
            commands::update_settings,
            commands::export_html,
            commands::export_pdf,
        ])
        .run(tauri::generate_context!())
        .expect("error while running InkFlow");
}

#[cfg(test)]
mod tests {
    #[test]
    fn main_capability_allows_the_window_close_flow() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json"))
                .expect("default capability should be valid JSON");
        let permissions = capability["permissions"]
            .as_array()
            .expect("default capability should list permissions");
        let has_permission = |expected: &str| {
            permissions
                .iter()
                .any(|permission| permission.as_str() == Some(expected))
        };

        assert!(
            has_permission("core:window:allow-destroy"),
            "onCloseRequested completes clean closes through Window.destroy"
        );
        assert!(
            has_permission("dialog:allow-message"),
            "dirty-document closes require the native confirmation dialog"
        );
    }
}
