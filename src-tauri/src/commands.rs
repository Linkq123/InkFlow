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
        OpenTargetRequest, RecoveryEntry, RecoverySnapshot, SaveDocumentRequest, SaveOutcome,
        SearchHit, SearchRequest, SessionV1, SettingsV1, WorkspaceSnapshot, WriteAssetRequest,
        WriteAssetResult,
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
    let recovery_dir = state.recovery.directory().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
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
    if let Some(filename) = resource.strip_prefix("inkflow-asset://") {
        let path = pending_asset_path(state.recovery.directory(), &document_id, filename)?;
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
    let document_path = state.documents.path_for(&document_id).ok_or_else(|| {
        ApiError::new(
            "document_not_found",
            "Save the document before loading relative images.",
        )
    })?;
    let workspace = state.workspace.current_root();
    asset::read_resource(&document_path, workspace.as_deref(), &resource)
}

fn pending_asset_path(
    recovery_dir: &Path,
    document_id: &str,
    filename: &str,
) -> ApiResult<PathBuf> {
    fn is_single_component(value: &str) -> bool {
        let mut components = Path::new(value).components();
        matches!(components.next(), Some(std::path::Component::Normal(_)))
            && components.next().is_none()
    }

    if !is_single_component(document_id) || !is_single_component(filename) {
        return Err(ApiError::new(
            "invalid_asset_path",
            "Pending image paths cannot contain directory components.",
        ));
    }
    let directory = recovery_dir.join("assets").join(document_id);
    let directory = crate::fileio::canonical_existing(&directory)?;
    let path = crate::fileio::canonical_existing(&directory.join(filename))?;
    let metadata = fs::metadata(&path)
        .map_err(|error| ApiError::io("Unable to inspect the pending image", error))?;
    if !path.starts_with(&directory) || !metadata.is_file() || metadata.len() > 50 * 1024 * 1024 {
        return Err(ApiError::new(
            "invalid_asset_path",
            "The pending image is outside its document scope or exceeds 50MB.",
        ));
    }
    let is_image = matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    );
    if !is_image {
        return Err(ApiError::new(
            "invalid_asset",
            "The resource is not a supported image.",
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::pending_asset_path;
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
pub async fn export_html(request: ExportRequest) -> ApiResult<ExportOutcome> {
    tauri::async_runtime::spawn_blocking(move || export::export_html(request))
        .await
        .map_err(|error| ApiError::new("html_export_error", error.to_string()))?
}

#[tauri::command]
pub async fn export_pdf(request: ExportRequest, window: WebviewWindow) -> ApiResult<ExportOutcome> {
    export::export_pdf(request, window).await
}
