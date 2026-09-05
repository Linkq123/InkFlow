use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use tauri::{State, WebviewWindow};

use crate::{
    AppState, asset,
    data_lock::lock_path_mutations,
    error::{ApiError, ApiResult},
    export,
    model::{
        CheckpointRequest, DocumentSnapshot, ExportOutcome, ExportRequest, ExternalChange,
        OpenTargetRequest, PreparedExportDestination, PreparedExportSource, RecoveryEntry,
        RecoverySnapshot, SaveDocumentRequest, SaveOutcome, SearchHit, SearchRequest, SessionV1,
        SettingsV1, WorkspaceSnapshot, WriteAssetRequest, WriteAssetResult,
    },
};

#[tauri::command]
pub fn take_startup_targets(state: State<'_, AppState>) -> Vec<OpenTargetRequest> {
    state.open_targets.lock().take_and_mark_ready()
}

#[tauri::command]
pub async fn open_paths(
    paths: Vec<String>,
    update_settings: bool,
    state: State<'_, AppState>,
) -> ApiResult<Vec<DocumentSnapshot>> {
    let documents = Arc::clone(&state.documents);
    let settings = Arc::clone(&state.settings);
    tauri::async_runtime::spawn_blocking(move || {
        let result = documents.open_paths(paths)?;
        if update_settings && !result.is_empty() {
            let mut current = settings.get();
            for document in result.iter().rev() {
                if let Some(path) = document.path.as_ref() {
                    current.recent_files.retain(|item| item != path);
                    current.recent_files.insert(0, path.clone());
                }
            }
            let _ = settings.update(current);
        }
        Ok(result)
    })
    .await
    .map_err(|error| ApiError::new("open_error", error.to_string()))?
}

#[tauri::command]
pub fn reload_document(
    document_id: String,
    state: State<'_, AppState>,
) -> ApiResult<DocumentSnapshot> {
    state.documents.reload(&document_id)
}

#[tauri::command]
pub fn close_document(document_id: String, state: State<'_, AppState>) {
    state.documents.close(&document_id);
}

#[tauri::command]
pub async fn save_document(
    request: SaveDocumentRequest,
    state: State<'_, AppState>,
) -> ApiResult<SaveOutcome> {
    let documents = Arc::clone(&state.documents);
    let recovery = Arc::clone(&state.recovery);
    let workspace = state.workspace.current_root();
    tauri::async_runtime::spawn_blocking(move || {
        documents.save(request, &recovery, None, workspace.as_deref())
    })
    .await
    .map_err(|error| ApiError::new("save_error", error.to_string()))?
}

#[tauri::command]
pub async fn save_document_as(
    request: SaveDocumentRequest,
    state: State<'_, AppState>,
) -> ApiResult<SaveOutcome> {
    let path =
        request.path.as_ref().map(PathBuf::from).ok_or_else(|| {
            ApiError::new("missing_output_path", "Choose a destination document.")
        })?;
    let documents = Arc::clone(&state.documents);
    let recovery = Arc::clone(&state.recovery);
    let workspace = state.workspace.current_root();
    tauri::async_runtime::spawn_blocking(move || {
        documents.save(request, &recovery, Some(path), workspace.as_deref())
    })
    .await
    .map_err(|error| ApiError::new("save_error", error.to_string()))?
}

#[tauri::command]
pub fn check_external_changes(state: State<'_, AppState>) -> Vec<ExternalChange> {
    state.documents.check_external_changes()
}

#[tauri::command]
pub async fn open_workspace(
    path: String,
    update_settings: bool,
    state: State<'_, AppState>,
) -> ApiResult<WorkspaceSnapshot> {
    let workspace = Arc::clone(&state.workspace);
    let settings = Arc::clone(&state.settings);
    tauri::async_runtime::spawn_blocking(move || {
        let snapshot = workspace.open(Path::new(&path))?;
        if update_settings {
            let mut current = settings.get();
            current
                .recent_workspaces
                .retain(|item| item != &snapshot.root);
            current.recent_workspaces.insert(0, snapshot.root.clone());
            current.show_file_tree = true;
            let _ = settings.update(current);
        }
        Ok(snapshot)
    })
    .await
    .map_err(|error| ApiError::new("workspace_error", error.to_string()))?
}

#[tauri::command]
pub fn refresh_workspace(state: State<'_, AppState>) -> ApiResult<Option<WorkspaceSnapshot>> {
    state.workspace.refresh()
}

#[tauri::command]
pub async fn search_workspace(
    request: SearchRequest,
    state: State<'_, AppState>,
) -> ApiResult<Vec<SearchHit>> {
    let workspace = Arc::clone(&state.workspace);
    tauri::async_runtime::spawn_blocking(move || workspace.search(request))
        .await
        .map_err(|error| ApiError::new("search_error", error.to_string()))?
}

#[tauri::command]
pub async fn create_workspace_entry(
    parent: String,
    name: String,
    is_dir: bool,
    state: State<'_, AppState>,
) -> ApiResult<WorkspaceSnapshot> {
    let workspace = Arc::clone(&state.workspace);
    let parent = PathBuf::from(parent);
    tauri::async_runtime::spawn_blocking(move || workspace.create_entry(&parent, &name, is_dir))
        .await
        .map_err(|error| ApiError::new("workspace_error", error.to_string()))?
}

#[tauri::command]
pub async fn rename_workspace_entry(
    path: String,
    new_name: String,
    state: State<'_, AppState>,
) -> ApiResult<WorkspaceSnapshot> {
    let workspace = Arc::clone(&state.workspace);
    let documents = Arc::clone(&state.documents);
    let path = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || {
        workspace.rename_entry_with(&path, &new_name, |source, destination, is_directory| {
            documents.relocate_paths(source, destination, is_directory);
        })
    })
    .await
    .map_err(|error| ApiError::new("workspace_error", error.to_string()))?
}

#[tauri::command]
pub async fn trash_workspace_entry(
    path: String,
    state: State<'_, AppState>,
) -> ApiResult<WorkspaceSnapshot> {
    let workspace = Arc::clone(&state.workspace);
    let path = PathBuf::from(path);
    tauri::async_runtime::spawn_blocking(move || workspace.trash_entry(&path))
        .await
        .map_err(|error| ApiError::new("workspace_error", error.to_string()))?
}

#[tauri::command]
pub async fn write_asset(
    request: WriteAssetRequest,
    state: State<'_, AppState>,
) -> ApiResult<WriteAssetResult> {
    let documents = Arc::clone(&state.documents);
    let recovery = Arc::clone(&state.recovery);
    let recovery_dir = state.recovery.directory().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let _recovery_guard = request
            .document_path
            .is_none()
            .then(|| recovery.guard_directory())
            .transpose()?;
        let path_lock = lock_path_mutations()?;
        if let Some(document_path) = request.document_path.as_deref() {
            let requested = crate::fileio::canonical_existing(Path::new(document_path))?;
            let known = documents.path_for(&request.document_id).ok_or_else(|| {
                ApiError::new("document_not_found", "The asset document is not open.")
            })?;
            if requested != known {
                return Err(ApiError::new(
                    "document_mismatch",
                    "The asset path does not belong to the selected document.",
                ));
            }
        }
        asset::write_asset_locked(&recovery_dir, request, &path_lock)
    })
    .await
    .map_err(|error| ApiError::new("asset_error", error.to_string()))?
}

#[tauri::command]
pub fn load_resource(
    document_id: String,
    resource: String,
    state: State<'_, AppState>,
) -> ApiResult<String> {
    let document_path = state.documents.path_for(&document_id);
    let workspace = state.workspace.current_root();
    let _recovery_guard = resource
        .starts_with("inkflow-asset://")
        .then(|| state.recovery.guard_directory())
        .transpose()?;
    load_resource_from_scope(
        state.recovery.directory(),
        &document_id,
        document_path.as_deref(),
        workspace.as_deref(),
        &resource,
    )
}

fn load_resource_from_scope(
    recovery_directory: &Path,
    document_id: &str,
    document_path: Option<&Path>,
    workspace_root: Option<&Path>,
    resource: &str,
) -> ApiResult<String> {
    if let Some(filename) = resource.strip_prefix("inkflow-asset://") {
        let path = asset::pending_asset_path(recovery_directory, document_id, filename)?;
        let bytes = fs::read(&path)
            .map_err(|error| ApiError::io("Unable to read the pending image", error))?;
        let mime = match path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
        {
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "svg" => "image/svg+xml",
            _ => "image/png",
        };
        return Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)));
    }
    let document_path = document_path.ok_or_else(|| {
        ApiError::new(
            "document_not_found",
            "Save the document before loading relative images.",
        )
    })?;
    asset::read_resource(document_path, workspace_root, resource)
}

#[cfg(test)]
mod tests {
    use crate::asset::pending_asset_path;
    use std::fs;

    #[test]
    fn pending_assets_cannot_escape_the_document_directory() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("assets").join("document");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("image.png"), b"image").unwrap();
        fs::write(temp.path().join("secret.png"), b"secret").unwrap();

        assert!(pending_asset_path(temp.path(), "document", "image.png").is_ok());
        assert!(pending_asset_path(temp.path(), "document", "../../secret.png").is_err());
        assert!(pending_asset_path(temp.path(), "../document", "image.png").is_err());
    }
}

#[tauri::command]
pub async fn checkpoint_document(
    request: CheckpointRequest,
    state: State<'_, AppState>,
) -> ApiResult<Option<RecoveryEntry>> {
    let recovery = Arc::clone(&state.recovery);
    tauri::async_runtime::spawn_blocking(move || recovery.checkpoint(request))
        .await
        .map_err(|error| ApiError::new("recovery_error", error.to_string()))?
}

#[tauri::command]
pub async fn list_recovery(state: State<'_, AppState>) -> ApiResult<Vec<RecoveryEntry>> {
    let recovery = Arc::clone(&state.recovery);
    tauri::async_runtime::spawn_blocking(move || recovery.list())
        .await
        .map_err(|error| ApiError::new("recovery_error", error.to_string()))?
}

#[tauri::command]
pub async fn restore_revision(
    id: String,
    state: State<'_, AppState>,
) -> ApiResult<RecoverySnapshot> {
    let recovery = Arc::clone(&state.recovery);
    tauri::async_runtime::spawn_blocking(move || recovery.restore(&id))
        .await
        .map_err(|error| ApiError::new("recovery_error", error.to_string()))?
}

#[tauri::command]
pub async fn delete_recovery(id: String, state: State<'_, AppState>) -> ApiResult<()> {
    let recovery = Arc::clone(&state.recovery);
    tauri::async_runtime::spawn_blocking(move || recovery.delete(&id).map(|_| ()))
        .await
        .map_err(|error| ApiError::new("recovery_error", error.to_string()))?
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsV1 {
    state.settings.get()
}

#[tauri::command]
pub async fn update_settings(
    settings: SettingsV1,
    state: State<'_, AppState>,
) -> ApiResult<SettingsV1> {
    let store = Arc::clone(&state.settings);
    tauri::async_runtime::spawn_blocking(move || store.update(settings))
        .await
        .map_err(|error| ApiError::new("settings_error", error.to_string()))?
}

#[tauri::command]
pub fn get_session(state: State<'_, AppState>) -> ApiResult<SessionV1> {
    state.session.get()
}

#[tauri::command]
pub async fn update_session(
    session: SessionV1,
    state: State<'_, AppState>,
) -> ApiResult<SessionV1> {
    let store = Arc::clone(&state.session);
    tauri::async_runtime::spawn_blocking(move || store.update(session))
        .await
        .map_err(|error| ApiError::new("session_error", error.to_string()))?
}

#[tauri::command]
pub async fn mark_performance_ready(state: State<'_, AppState>) -> ApiResult<bool> {
    let Some(path) = state.performance_marker.clone() else {
        return Ok(false);
    };
    tauri::async_runtime::spawn_blocking(move || {
        crate::fileio::atomic_write(&path, b"ready")?;
        Ok(true)
    })
    .await
    .map_err(|error| ApiError::new("performance_marker_error", error.to_string()))?
}

#[tauri::command]
pub async fn prepare_export_destination(
    path: String,
    state: State<'_, AppState>,
) -> ApiResult<PreparedExportDestination> {
    let exports = Arc::clone(&state.exports);
    tauri::async_runtime::spawn_blocking(move || exports.prepare(Path::new(&path)))
        .await
        .map_err(|error| ApiError::new("export_prepare_error", error.to_string()))?
}

#[tauri::command]
pub async fn prepare_export_source(
    document_id: String,
    document_path: Option<String>,
    workspace_root: Option<String>,
    state: State<'_, AppState>,
) -> ApiResult<PreparedExportSource> {
    let expected_document_path = document_path.map(PathBuf::from);
    let expected_workspace_root = workspace_root.map(PathBuf::from);
    let recovery_root = state.recovery.directory().to_path_buf();
    let documents = Arc::clone(&state.documents);
    let workspace = Arc::clone(&state.workspace);
    let recovery = Arc::clone(&state.recovery);
    let exports = Arc::clone(&state.exports);
    tauri::async_runtime::spawn_blocking(move || {
        {
            let _path_guard = lock_path_mutations()?;
            if !export_source_snapshot_is_current(
                &documents,
                &workspace,
                &document_id,
                &expected_document_path,
                &expected_workspace_root,
            ) {
                return Err(export_source_snapshot_changed());
            }
        }
        let _recovery_guard = recovery
            .guard_directory()
            .map_err(export::invalid_export_source)?;
        let prepared = exports.prepare_source(
            document_id.clone(),
            expected_document_path.clone(),
            expected_workspace_root.clone(),
            recovery_root,
        )?;

        let _path_guard = match lock_path_mutations() {
            Ok(guard) => guard,
            Err(error) => {
                exports.cancel_source(&prepared.token);
                return Err(error);
            }
        };
        if !export_source_snapshot_is_current(
            &documents,
            &workspace,
            &document_id,
            &expected_document_path,
            &expected_workspace_root,
        ) {
            exports.cancel_source(&prepared.token);
            return Err(export_source_snapshot_changed());
        }
        Ok(prepared)
    })
    .await
    .map_err(|error| ApiError::new("export_prepare_error", error.to_string()))?
}

fn export_source_snapshot_is_current(
    documents: &crate::document::DocumentStore,
    workspace: &crate::workspace::WorkspaceStore,
    document_id: &str,
    expected_document_path: &Option<PathBuf>,
    expected_workspace_root: &Option<PathBuf>,
) -> bool {
    documents.path_for(document_id).as_ref() == expected_document_path.as_ref()
        && workspace.current_root().as_ref() == expected_workspace_root.as_ref()
}

fn export_source_snapshot_changed() -> ApiError {
    ApiError::new(
        "invalid_export_source",
        "The document path or workspace changed before the export resource scope was prepared.",
    )
}

#[tauri::command]
pub async fn load_export_resource(
    source_token: String,
    resource: String,
    state: State<'_, AppState>,
) -> ApiResult<String> {
    let exports = Arc::clone(&state.exports);
    let recovery_root = state.recovery.directory().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let scope = exports.source(&source_token)?;
        if let Some(data_url) = scope.load_pending_asset(&resource)? {
            return Ok(data_url);
        }
        let _source_guards = scope.guard_resource(&resource)?;
        load_resource_from_scope(
            &recovery_root,
            &scope.document_id,
            scope.document_path.as_deref(),
            scope.workspace_root.as_deref(),
            &resource,
        )
    })
    .await
    .map_err(|error| ApiError::new("export_resource_error", error.to_string()))?
}

#[tauri::command]
pub fn cancel_export_source(source_token: String, state: State<'_, AppState>) {
    state.exports.cancel_source(&source_token);
}

#[tauri::command]
pub fn cancel_export_destination(token: String, state: State<'_, AppState>) {
    state.exports.cancel(&token);
}

#[tauri::command]
pub async fn export_html(
    request: ExportRequest,
    destination_token: String,
    state: State<'_, AppState>,
) -> ApiResult<ExportOutcome> {
    let prepared = state
        .exports
        .take(&destination_token, request.output_path.as_deref())?;
    tauri::async_runtime::spawn_blocking(move || export::export_html_prepared(request, prepared))
        .await
        .map_err(|error| ApiError::new("html_export_error", error.to_string()))?
}

#[tauri::command]
pub async fn export_pdf(
    request: ExportRequest,
    destination_token: String,
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> ApiResult<ExportOutcome> {
    let prepared = state
        .exports
        .take(&destination_token, request.output_path.as_deref())?;
    export::export_pdf_prepared(request, window, prepared).await
}
