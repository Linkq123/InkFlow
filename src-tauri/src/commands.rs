use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use tauri::{State, WebviewWindow};

use crate::{
    AppState, asset,
    error::{ApiError, ApiResult},
    export,
    model::{
        CheckpointRequest, DocumentSnapshot, ExportOutcome, ExportRequest, ExternalChange,
        RecoveryEntry, RecoverySnapshot, SaveDocumentRequest, SaveOutcome, SearchHit,
        SearchRequest, SessionV1, SettingsV1, WorkspaceSnapshot, WriteAssetRequest,
        WriteAssetResult,
    },
};

#[tauri::command]
pub fn take_startup_paths(state: State<'_, AppState>) -> Vec<String> {
    std::mem::take(&mut *state.startup_paths.lock())
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
pub fn save_document(
    request: SaveDocumentRequest,
    state: State<'_, AppState>,
) -> ApiResult<SaveOutcome> {
    let workspace = state.workspace.current_root();
    state
        .documents
        .save(request, &state.recovery, None, workspace.as_deref())
}

#[tauri::command]
pub fn save_document_as(
    request: SaveDocumentRequest,
    state: State<'_, AppState>,
) -> ApiResult<SaveOutcome> {
    let path =
        request.path.as_ref().map(PathBuf::from).ok_or_else(|| {
            ApiError::new("missing_output_path", "Choose a destination document.")
        })?;
    let workspace = state.workspace.current_root();
    state
        .documents
        .save(request, &state.recovery, Some(path), workspace.as_deref())
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
pub fn create_workspace_entry(
    parent: String,
    name: String,
    is_dir: bool,
    state: State<'_, AppState>,
) -> ApiResult<WorkspaceSnapshot> {
    state
        .workspace
        .create_entry(Path::new(&parent), &name, is_dir)
}

#[tauri::command]
pub fn rename_workspace_entry(
    path: String,
    new_name: String,
    state: State<'_, AppState>,
) -> ApiResult<WorkspaceSnapshot> {
    let source = crate::fileio::canonical_existing(Path::new(&path))?;
    let destination = source
        .parent()
        .ok_or_else(|| ApiError::new("invalid_path", "Cannot rename the workspace root."))?
        .join(&new_name);
    let is_directory = source.is_dir();
    let snapshot = state.workspace.rename_entry(&source, &new_name)?;
    let destination = crate::fileio::canonical_existing(&destination)?;
    state
        .documents
        .relocate_paths(&source, &destination, is_directory);
    Ok(snapshot)
}

#[tauri::command]
pub fn trash_workspace_entry(
    path: String,
    state: State<'_, AppState>,
) -> ApiResult<WorkspaceSnapshot> {
    state.workspace.trash_entry(Path::new(&path))
}

#[tauri::command]
pub fn write_asset(
    request: WriteAssetRequest,
    state: State<'_, AppState>,
) -> ApiResult<WriteAssetResult> {
    if let Some(document_path) = request.document_path.as_deref() {
        let requested = crate::fileio::canonical_existing(Path::new(document_path))?;
        let known = state
            .documents
            .path_for(&request.document_id)
            .ok_or_else(|| {
                ApiError::new("document_not_found", "The asset document is not open.")
            })?;
        if requested != known {
            return Err(ApiError::new(
                "document_mismatch",
                "The asset path does not belong to the selected document.",
            ));
        }
    }
    asset::write_asset(state.recovery.directory(), request)
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
pub fn checkpoint_document(
    request: CheckpointRequest,
    state: State<'_, AppState>,
) -> ApiResult<Option<RecoveryEntry>> {
    state.recovery.checkpoint(request)
}

#[tauri::command]
pub async fn list_recovery(state: State<'_, AppState>) -> ApiResult<Vec<RecoveryEntry>> {
    let recovery = Arc::clone(&state.recovery);
    tauri::async_runtime::spawn_blocking(move || recovery.list())
        .await
        .map_err(|error| ApiError::new("recovery_error", error.to_string()))?
}

#[tauri::command]
pub fn restore_revision(id: String, state: State<'_, AppState>) -> ApiResult<RecoverySnapshot> {
    state.recovery.restore(&id)
}

#[tauri::command]
pub fn delete_recovery(id: String, state: State<'_, AppState>) -> ApiResult<()> {
    state.recovery.delete(&id)
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsV1 {
    state.settings.get()
}

#[tauri::command]
pub fn update_settings(settings: SettingsV1, state: State<'_, AppState>) -> ApiResult<SettingsV1> {
    state.settings.update(settings)
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
pub fn export_html(request: ExportRequest) -> ApiResult<ExportOutcome> {
    export::export_html(request)
}

#[tauri::command]
pub async fn export_pdf(request: ExportRequest, window: WebviewWindow) -> ApiResult<ExportOutcome> {
    export::export_pdf(request, window).await
}
