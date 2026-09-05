#![cfg_attr(all(feature = "cli", not(feature = "desktop")), allow(dead_code))]

mod asset;
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "desktop")]
mod commands;
mod data_lock;
#[cfg(any(feature = "cli", feature = "desktop"))]
mod destination;
mod document;
mod encoding;
mod error;
mod export;
mod fileio;
pub mod model;
mod recovery;
#[cfg(feature = "desktop")]
mod renderer;
mod session;
mod settings;
mod workspace;

use std::path::PathBuf;

pub(crate) const DESKTOP_OPEN_WORKSPACE_FLAG: &str = "--inkflow-open-workspace";
pub(crate) const RENDERER_PROTOCOL: &str = "inkflow.renderer/v3";

#[cfg(feature = "desktop")]
use std::sync::Arc;

use directories::ProjectDirs;
#[cfg(feature = "desktop")]
use parking_lot::Mutex;
#[cfg(feature = "desktop")]
use tauri::{Emitter, Manager};

#[cfg(feature = "desktop")]
use document::DocumentStore;
#[cfg(feature = "desktop")]
use export::ExportDestinationStore;
#[cfg(feature = "desktop")]
use model::OpenTargetRequest;
#[cfg(feature = "desktop")]
use recovery::RecoveryStore;
#[cfg(feature = "desktop")]
use session::SessionStore;
#[cfg(feature = "desktop")]
use settings::SettingsStore;
#[cfg(feature = "desktop")]
use workspace::WorkspaceStore;

const PERFORMANCE_PROFILE_ENV: &str = "INKFLOW_PERFORMANCE_PROFILE";
const PERFORMANCE_READY_MARKER: &str = "performance-ready";

pub fn default_application_data_directory() -> Result<PathBuf, std::io::Error> {
    if let Some(value) = std::env::var_os(PERFORMANCE_PROFILE_ENV) {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{PERFORMANCE_PROFILE_ENV} must contain an absolute path"),
            ));
        }
        return Ok(path);
    }
    ProjectDirs::from("com", "InkFlow", "InkFlow")
        .map(|project| project.data_local_dir().to_path_buf())
        .ok_or_else(|| {
            std::io::Error::other("Unable to resolve the InkFlow application data directory")
        })
}

#[cfg(feature = "desktop")]
pub struct AppState {
    documents: Arc<DocumentStore>,
    workspace: Arc<WorkspaceStore>,
    recovery: Arc<RecoveryStore>,
    settings: Arc<SettingsStore>,
    session: Arc<SessionStore>,
    exports: Arc<ExportDestinationStore>,
    performance_marker: Option<PathBuf>,
    open_targets: Arc<Mutex<OpenTargetBuffer>>,
}

#[cfg(feature = "desktop")]
#[derive(Debug, Default)]
struct OpenTargetBuffer {
    events_ready: bool,
    queued: Vec<OpenTargetRequest>,
}

#[cfg(feature = "desktop")]
impl OpenTargetBuffer {
    fn new(queued: Vec<OpenTargetRequest>) -> Self {
        Self {
            events_ready: false,
            queued,
        }
    }

    fn route(&mut self, requests: Vec<OpenTargetRequest>) -> Option<Vec<OpenTargetRequest>> {
        if self.events_ready {
            Some(requests)
        } else {
            self.queued.extend(requests);
            None
        }
    }

    fn take_and_mark_ready(&mut self) -> Vec<OpenTargetRequest> {
        self.events_ready = true;
        std::mem::take(&mut self.queued)
    }
}

#[cfg(feature = "desktop")]
#[derive(Debug, Default, PartialEq, Eq)]
struct StartupTargets {
    paths: Vec<String>,
    workspace: Option<String>,
}

#[cfg(feature = "desktop")]
impl StartupTargets {
    fn into_requests(self) -> Vec<OpenTargetRequest> {
        let mut requests = Vec::with_capacity(2);
        if let Some(path) = self.workspace {
            requests.push(OpenTargetRequest::Workspace { path });
        }
        if !self.paths.is_empty() {
            requests.push(OpenTargetRequest::Paths { paths: self.paths });
        }
        requests
    }
}

#[cfg(feature = "desktop")]
fn parse_startup_targets(
    args: impl IntoIterator<Item = String>,
    current_directory: &std::path::Path,
) -> StartupTargets {
    fn resolve(
        value: String,
        current_directory: &std::path::Path,
        directory: bool,
    ) -> Option<String> {
        let path = PathBuf::from(value);
        let path = if path.is_absolute() {
            path
        } else {
            current_directory.join(path)
        };
        let path = dunce::canonicalize(path).ok()?;
        if (directory && !path.is_dir()) || (!directory && !path.is_file()) {
            return None;
        }
        Some(path.to_string_lossy().into_owned())
    }

    let mut targets = StartupTargets::default();
    let mut args = args.into_iter().skip(1);
    while let Some(value) = args.next() {
        if value == DESKTOP_OPEN_WORKSPACE_FLAG {
            if let Some(workspace) = args
                .next()
                .and_then(|path| resolve(path, current_directory, true))
            {
                targets.workspace = Some(workspace);
            }
        } else if !value.starts_with('-') {
            if let Some(path) = resolve(value, current_directory, false) {
                targets.paths.push(path);
            }
        }
    }
    targets
}

#[cfg(feature = "desktop")]
pub fn run() {
    if let Some(launch) = renderer::launch_from_args() {
        renderer::run(launch);
        return;
    }
    let startup_directory = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let startup_requests =
        parse_startup_targets(std::env::args(), &startup_directory).into_requests();
    let open_targets = Arc::new(Mutex::new(OpenTargetBuffer::new(startup_requests)));
    let callback_open_targets = Arc::clone(&open_targets);
    let state_open_targets = Arc::clone(&open_targets);
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(move |app, args, cwd| {
            let requests = parse_startup_targets(args, std::path::Path::new(&cwd)).into_requests();
            let ready_requests = if requests.is_empty() {
                None
            } else {
                callback_open_targets.lock().route(requests)
            };
            for request in ready_requests.into_iter().flatten() {
                match request {
                    OpenTargetRequest::Workspace { path } => {
                        let _ = app.emit("app-open-workspace", path);
                    }
                    OpenTargetRequest::Paths { paths } => {
                        let _ = app.emit("app-open-paths", paths);
                    }
                }
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
        .setup(move |app| {
            let data = default_application_data_directory()?;
            std::fs::create_dir_all(&data)?;
            let performance_marker = std::env::var_os(PERFORMANCE_PROFILE_ENV)
                .map(|_| data.join(PERFORMANCE_READY_MARKER));
            app.manage(AppState {
                documents: Arc::new(DocumentStore::new()),
                workspace: Arc::new(WorkspaceStore::new()),
                recovery: Arc::new(RecoveryStore::new(data.join("Recovery"))?),
                settings: Arc::new(SettingsStore::load(data.join("settings.json"))),
                session: Arc::new(SessionStore::load(data.join("session.json"))),
                exports: Arc::new(ExportDestinationStore::new()),
                performance_marker,
                open_targets: state_open_targets,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_paths,
            commands::take_startup_targets,
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
            commands::get_session,
            commands::update_session,
            commands::mark_performance_ready,
            commands::prepare_export_source,
            commands::load_export_resource,
            commands::cancel_export_source,
            commands::prepare_export_destination,
            commands::cancel_export_destination,
            commands::export_html,
            commands::export_pdf,
        ])
        .run(tauri::generate_context!())
        .expect("error while running InkFlow");
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "desktop")]
    use super::{DESKTOP_OPEN_WORKSPACE_FLAG, OpenTargetBuffer, parse_startup_targets};
    #[cfg(feature = "desktop")]
    use crate::model::OpenTargetRequest;

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

    #[cfg(feature = "desktop")]
    #[test]
    fn startup_targets_keep_workspaces_separate_from_document_paths() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let document = temp.path().join("note.md");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(&document, "note").unwrap();

        let targets = parse_startup_targets(
            vec![
                "InkFlow.exe".into(),
                DESKTOP_OPEN_WORKSPACE_FLAG.into(),
                "workspace".into(),
                "note.md".into(),
                "workspace".into(),
            ],
            temp.path(),
        );
        let expected_workspace = dunce::canonicalize(&workspace)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        assert_eq!(
            targets.workspace.as_deref(),
            Some(expected_workspace.as_str())
        );
        assert_eq!(
            targets.paths,
            vec![
                dunce::canonicalize(document)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            ]
        );
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn open_target_buffer_closes_the_listener_registration_gap() {
        let initial = OpenTargetRequest::Paths {
            paths: vec!["initial.md".into()],
        };
        let during_startup = OpenTargetRequest::Workspace {
            path: r"C:\Notes".into(),
        };
        let after_ready = OpenTargetRequest::Paths {
            paths: vec!["later.md".into()],
        };
        let mut buffer = OpenTargetBuffer::new(vec![initial.clone()]);

        assert_eq!(buffer.route(vec![during_startup.clone()]), None);
        assert_eq!(buffer.take_and_mark_ready(), vec![initial, during_startup]);
        assert_eq!(
            buffer.route(vec![after_ready.clone()]),
            Some(vec![after_ready])
        );
        assert!(buffer.take_and_mark_ready().is_empty());
    }
}
