use std::{path::Path, sync::OnceLock};

#[cfg(feature = "desktop")]
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(all(feature = "desktop", target_os = "windows"))]
use std::time::SystemTime;

use base64::{Engine, engine::general_purpose::STANDARD};
#[cfg(feature = "desktop")]
use parking_lot::Mutex;

use crate::{
    data_lock::lock_path_mutations,
    error::{ApiError, ApiResult},
    fileio::{
        AtomicWriteOutcome, atomic_create_if_absent, atomic_replace_existing, atomic_write,
        atomic_write_if_revision,
    },
    model::{DiskRevision, ExportRequest},
};

#[cfg(feature = "desktop")]
use crate::{
    asset,
    data_lock::DataLock,
    destination::DestinationSnapshot,
    fileio::{
        DirectoryIdentityGuard, FileIdentity, canonical_existing, directory_identity,
        guard_directory_identity, is_symbolic_link_or_junction, revision, revision_from_bytes,
    },
    model::{ExportOutcome, PreparedExportDestination, PreparedExportSource},
};

#[cfg(feature = "desktop")]
const EXPORT_DESTINATION_TTL: Duration = Duration::from_secs(10 * 60);
#[cfg(feature = "desktop")]
const MAX_PREPARED_EXPORT_DESTINATIONS: usize = 16;

#[cfg(all(feature = "desktop", target_os = "windows"))]
const PDF_TEMPORARY_DIRECTORY_PREFIX: &str = "InkFlow-pdf-";
#[cfg(all(feature = "desktop", target_os = "windows"))]
const PDF_TEMPORARY_DIRECTORY_NAME_LENGTH: usize = PDF_TEMPORARY_DIRECTORY_PREFIX.len() + 32;
#[cfg(all(feature = "desktop", target_os = "windows"))]
const STALE_PDF_TEMPORARY_DIRECTORY_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn active_pdf_temporary_directories() -> &'static Mutex<HashSet<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(HashSet::new()))
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
struct PdfTemporaryLease {
    directory: PathBuf,
    temporary: PathBuf,
    directory_guard: Option<DirectoryIdentityGuard>,
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
impl PdfTemporaryLease {
    fn create() -> ApiResult<Arc<Self>> {
        let temp_root = canonical_existing(&std::env::temp_dir())?;
        if !temp_root.is_dir() {
            return Err(ApiError::new(
                "pdf_export_error",
                "The system temporary directory is unavailable.",
            ));
        }
        let temp_root_identity = directory_identity(&temp_root)?;
        let _temp_root_guard = guard_directory_identity(&temp_root, temp_root_identity)?;
        cleanup_stale_pdf_temporary_directories(&temp_root);
        for _ in 0..8 {
            let candidate = temp_root.join(format!(
                "{PDF_TEMPORARY_DIRECTORY_PREFIX}{}",
                uuid::Uuid::new_v4().simple()
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(ApiError::io(
                        "Unable to create the private PDF render directory",
                        error,
                    ));
                }
            }
            if is_symbolic_link_or_junction(&candidate)? {
                let _ = fs::remove_dir(&candidate);
                continue;
            }
            let directory = canonical_existing(&candidate)?;
            if directory.parent() != Some(temp_root.as_path()) {
                let _ = fs::remove_dir(&candidate);
                continue;
            }
            let identity = directory_identity(&directory)?;
            let directory_guard = guard_directory_identity(&directory, identity)?;
            if is_symbolic_link_or_junction(&candidate)?
                || canonical_existing(&candidate)? != directory
            {
                drop(directory_guard);
                let _ = fs::remove_dir(&candidate);
                continue;
            }
            let temporary = directory.join("render.tmp.pdf");
            let lease = Arc::new(Self {
                directory: directory.clone(),
                temporary,
                directory_guard: Some(directory_guard),
            });
            active_pdf_temporary_directories().lock().insert(directory);
            return Ok(lease);
        }
        Err(ApiError::new(
            "pdf_export_error",
            "Unable to reserve a private PDF render directory.",
        ))
    }

    fn path(&self) -> &Path {
        &self.temporary
    }
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
impl Drop for PdfTemporaryLease {
    fn drop(&mut self) {
        // Keep the no-delete-share directory handle alive while resolving the
        // child path, so the private directory cannot be rebound to a junction
        // between the late WebView callback and cleanup.
        let _ = fs::remove_file(&self.temporary);
        self.directory_guard.take();
        let _ = fs::remove_dir(&self.directory);
        active_pdf_temporary_directories()
            .lock()
            .remove(&self.directory);
    }
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn cleanup_stale_pdf_temporary_directories(parent: &Path) {
    cleanup_stale_pdf_temporary_directories_at(parent, SystemTime::now());
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn cleanup_stale_pdf_temporary_directories_at(parent: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let active = active_pdf_temporary_directories().lock();
    for entry in entries.flatten() {
        let directory = entry.path();
        if active.contains(&directory) || !is_safe_pdf_temporary_directory(parent, &directory) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age >= STALE_PDF_TEMPORARY_DIRECTORY_AGE {
            let _ = fs::remove_dir_all(directory);
        }
    }
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn is_safe_pdf_temporary_directory(parent: &Path, directory: &Path) -> bool {
    directory.parent() == Some(parent)
        && directory
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(is_pdf_temporary_directory_name)
        && directory
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        && is_symbolic_link_or_junction(directory).is_ok_and(|is_link| !is_link)
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn is_pdf_temporary_directory_name(name: &str) -> bool {
    if name.len() != PDF_TEMPORARY_DIRECTORY_NAME_LENGTH {
        return false;
    }
    let Some(identifier) = name.strip_prefix(PDF_TEMPORARY_DIRECTORY_PREFIX) else {
        return false;
    };
    let bytes = identifier.as_bytes();
    bytes
        .iter()
        .all(|value| value.is_ascii_digit() || matches!(value, b'a'..=b'f'))
        && bytes[12] == b'4'
        && matches!(bytes[16], b'8' | b'9' | b'a' | b'b')
}

#[derive(Debug, Clone, Default)]
pub struct ExportWriteGuard {
    pub expected_revision: Option<DiskRevision>,
    pub create_only: bool,
    pub require_existing: bool,
}

#[cfg(feature = "desktop")]
struct StoredExportDestination {
    created_at: Instant,
    operation: PreparedExportOperation,
}

#[cfg(feature = "desktop")]
struct StoredExportSource {
    scope: ExportSourceScope,
}

#[cfg(feature = "desktop")]
#[derive(Debug, Clone)]
pub struct ExportSourceScope {
    pub document_id: String,
    pub document_path: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    document_directory: Option<ExportSourceDirectory>,
    workspace_directory: Option<ExportSourceDirectory>,
    recovery_directory: ExportSourceDirectory,
    recovery_asset_directory: Option<ExportSourceDirectory>,
    pending_asset_revisions: HashMap<String, DiskRevision>,
}

#[cfg(feature = "desktop")]
#[derive(Debug, Clone)]
struct ExportSourceDirectory {
    path: PathBuf,
    identity: FileIdentity,
}

#[cfg(feature = "desktop")]
pub(crate) struct ExportSourceGuards {
    _directories: Vec<DirectoryIdentityGuard>,
}

#[cfg(feature = "desktop")]
impl ExportSourceDirectory {
    fn capture(path: &Path) -> ApiResult<Self> {
        let path = canonical_existing(path)?;
        if !path.is_dir() {
            return Err(ApiError::new(
                "invalid_export_source",
                "An export resource directory is no longer available.",
            ));
        }
        let identity = directory_identity(&path)?;
        Ok(Self { path, identity })
    }

    fn guard(&self) -> ApiResult<DirectoryIdentityGuard> {
        let current = canonical_existing(&self.path).map_err(invalid_export_source)?;
        if current != self.path {
            return Err(invalid_export_source(ApiError::new(
                "path_changed",
                "An export resource directory resolved to a different path.",
            )));
        }
        guard_directory_identity(&current, self.identity).map_err(invalid_export_source)
    }
}

#[cfg(feature = "desktop")]
impl ExportSourceScope {
    pub(crate) fn guard_resource(&self, resource: &str) -> ApiResult<ExportSourceGuards> {
        let mut snapshots = Vec::new();
        if let Some(filename) = resource.strip_prefix("inkflow-asset://") {
            if !self.pending_asset_revisions.contains_key(filename) {
                return Err(ApiError::new(
                    "invalid_export_source",
                    "The pending export asset was not present in the prepared source scope.",
                ));
            }
            snapshots.push(&self.recovery_directory);
            snapshots.extend(self.recovery_asset_directory.iter());
        } else {
            snapshots.extend(self.document_directory.iter());
            snapshots.extend(self.workspace_directory.iter());
        }
        let mut seen = HashSet::new();
        let mut directories = Vec::with_capacity(snapshots.len());
        for snapshot in snapshots {
            if seen.insert(snapshot.path.clone()) {
                directories.push(snapshot.guard()?);
            }
        }
        Ok(ExportSourceGuards {
            _directories: directories,
        })
    }

    pub(crate) fn load_pending_asset(&self, resource: &str) -> ApiResult<Option<String>> {
        let Some(filename) = resource.strip_prefix("inkflow-asset://") else {
            return Ok(None);
        };
        let expected = self.pending_asset_revisions.get(filename).ok_or_else(|| {
            ApiError::new(
                "resource_not_found",
                "The pending export image was not available when the export began.",
            )
        })?;
        // Validate and hold the prepared directory identities before creating
        // or opening anything. Otherwise a rebound Recovery path could receive
        // InkFlow's lock file before the identity mismatch is discovered.
        let _source_guards = self.guard_resource(resource)?;
        let recovery_root = &self.recovery_directory.path;
        let _recovery_lock = DataLock::acquire(&recovery_root.join(".recovery.lock"))
            .map_err(invalid_export_source)?;
        // Revalidate after lock acquisition as well. The first guards remain
        // alive through this check and the following read, preventing directory
        // replacement on Windows while the prepared asset is consumed.
        let _locked_source_guards = self.guard_resource(resource)?;
        let path = asset::pending_asset_path(recovery_root, &self.document_id, filename)
            .map_err(invalid_export_source)?;
        let bytes = fs::read(&path)
            .map_err(|error| ApiError::io("Unable to read the pending export image", error))
            .map_err(invalid_export_source)?;
        let current = revision_from_bytes(&path, &bytes).map_err(invalid_export_source)?;
        if &current != expected {
            return Err(ApiError::new(
                "invalid_export_source",
                "The pending export asset changed after the source scope was prepared.",
            ));
        }
        let mime = match path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            "svg" => "image/svg+xml",
            _ => "image/png",
        };
        Ok(Some(format!(
            "data:{mime};base64,{}",
            STANDARD.encode(bytes)
        )))
    }
}

#[cfg(feature = "desktop")]
pub(crate) fn invalid_export_source(error: ApiError) -> ApiError {
    ApiError::new(
        "invalid_export_source",
        format!("The export resource scope changed: {}", error.message),
    )
}

#[cfg(feature = "desktop")]
#[derive(Debug)]
pub struct PreparedExportOperation {
    destination: DestinationSnapshot,
    write_guard: ExportWriteGuard,
}

#[cfg(feature = "desktop")]
impl PreparedExportOperation {
    pub fn path(&self) -> &Path {
        self.destination.path()
    }
}

#[cfg(feature = "desktop")]
pub struct ExportDestinationStore {
    pending: Mutex<HashMap<String, StoredExportDestination>>,
    sources: Mutex<HashMap<String, StoredExportSource>>,
    ttl: Duration,
    capacity: usize,
}

#[cfg(feature = "desktop")]
impl Default for ExportDestinationStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "desktop")]
impl ExportDestinationStore {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            sources: Mutex::new(HashMap::new()),
            ttl: EXPORT_DESTINATION_TTL,
            capacity: MAX_PREPARED_EXPORT_DESTINATIONS,
        }
    }

    #[cfg(test)]
    fn with_policy(ttl: Duration, capacity: usize) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            sources: Mutex::new(HashMap::new()),
            ttl,
            capacity,
        }
    }

    pub fn prepare(&self, path: &Path) -> ApiResult<PreparedExportDestination> {
        let (destination, expected_revision) = DestinationSnapshot::capture_file(path)?;
        let write_guard = ExportWriteGuard {
            create_only: expected_revision.is_none(),
            expected_revision,
            require_existing: false,
        };
        let token = uuid::Uuid::new_v4().to_string();
        let prepared = PreparedExportDestination {
            token: token.clone(),
            path: destination.path().to_string_lossy().into_owned(),
        };
        let mut pending = self.pending.lock();
        pending.retain(|_, item| item.created_at.elapsed() <= self.ttl);
        if pending.len() >= self.capacity {
            return Err(ApiError::new(
                "too_many_export_destinations",
                "Too many export destinations are waiting to be used.",
            ));
        }
        pending.insert(
            token,
            StoredExportDestination {
                created_at: Instant::now(),
                operation: PreparedExportOperation {
                    destination,
                    write_guard,
                },
            },
        );
        Ok(prepared)
    }

    pub fn take(
        &self,
        token: &str,
        requested_path: Option<&str>,
    ) -> ApiResult<PreparedExportOperation> {
        let stored = self.pending.lock().remove(token).ok_or_else(|| {
            ApiError::new(
                "invalid_export_token",
                "The export destination token is invalid or was already used.",
            )
        })?;
        if stored.created_at.elapsed() > self.ttl {
            return Err(ApiError::new(
                "expired_export_token",
                "The export destination confirmation expired.",
            ));
        }
        let requested_path = requested_path.ok_or_else(|| {
            ApiError::new(
                "missing_output_path",
                "The export request has no destination path.",
            )
        })?;
        if Path::new(requested_path) != stored.operation.path() {
            return Err(ApiError::new(
                "invalid_export_token",
                "The export destination does not match its confirmation token.",
            ));
        }
        Ok(stored.operation)
    }

    pub fn cancel(&self, token: &str) {
        self.pending.lock().remove(token);
    }

    pub fn prepare_source(
        &self,
        document_id: String,
        document_path: Option<PathBuf>,
        workspace_root: Option<PathBuf>,
        recovery_root: PathBuf,
    ) -> ApiResult<PreparedExportSource> {
        let _recovery_lock = DataLock::acquire(&recovery_root.join(".recovery.lock"))?;
        let document_directory = document_path
            .as_deref()
            .and_then(Path::parent)
            .map(ExportSourceDirectory::capture)
            .transpose()?;
        let workspace_directory = match (document_path.as_deref(), workspace_root.as_deref()) {
            (Some(document), Some(root)) if document.starts_with(root) => {
                Some(ExportSourceDirectory::capture(root)?)
            }
            _ => None,
        };
        let recovery_directory = ExportSourceDirectory::capture(&recovery_root)?;
        let (recovery_asset_directory, pending_asset_revisions) =
            capture_pending_export_assets(&recovery_root, &recovery_directory, &document_id)?;

        let token = uuid::Uuid::new_v4().to_string();
        let mut sources = self.sources.lock();
        if sources.len() >= self.capacity {
            return Err(ApiError::new(
                "too_many_export_sources",
                "Too many export resource scopes are waiting to be released.",
            ));
        }
        sources.insert(
            token.clone(),
            StoredExportSource {
                scope: ExportSourceScope {
                    document_id,
                    document_path,
                    workspace_root,
                    document_directory,
                    workspace_directory,
                    recovery_directory,
                    recovery_asset_directory,
                    pending_asset_revisions,
                },
            },
        );
        Ok(PreparedExportSource { token })
    }

    pub fn source(&self, token: &str) -> ApiResult<ExportSourceScope> {
        let sources = self.sources.lock();
        let Some(stored) = sources.get(token) else {
            return Err(ApiError::new(
                "invalid_export_source",
                "The export resource scope is invalid or was released.",
            ));
        };
        Ok(stored.scope.clone())
    }

    pub fn cancel_source(&self, token: &str) {
        self.sources.lock().remove(token);
    }
}

#[cfg(feature = "desktop")]
fn capture_pending_export_assets(
    recovery_root: &Path,
    recovery_directory: &ExportSourceDirectory,
    document_id: &str,
) -> ApiResult<(Option<ExportSourceDirectory>, HashMap<String, DiskRevision>)> {
    let mut components = Path::new(document_id).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(ApiError::new(
            "invalid_export_source",
            "The export document identifier is not a safe path component.",
        ));
    }

    let pending = recovery_root.join("assets").join(document_id);
    if !pending.exists() {
        return Ok((None, HashMap::new()));
    }
    let directory = ExportSourceDirectory::capture(&pending).map_err(invalid_export_source)?;
    let assets_root =
        canonical_existing(&recovery_root.join("assets")).map_err(invalid_export_source)?;
    if !assets_root.starts_with(&recovery_directory.path)
        || directory.path == assets_root
        || !directory.path.starts_with(&assets_root)
    {
        return Err(ApiError::new(
            "invalid_export_source",
            "The pending export asset directory is outside the recovery scope.",
        ));
    }

    let mut revisions = HashMap::new();
    for entry in fs::read_dir(&directory.path)
        .map_err(|error| ApiError::io("Unable to scan pending export assets", error))?
    {
        let entry = entry
            .map_err(|error| ApiError::io("Unable to inspect a pending export asset", error))?;
        let path = entry.path();
        if !path.is_file() || !asset::is_image_path(&path) {
            continue;
        }
        let filename = entry.file_name().into_string().map_err(|_| {
            ApiError::new(
                "invalid_export_source",
                "A pending export asset name is not valid Unicode.",
            )
        })?;
        let resolved = asset::pending_asset_path(recovery_root, document_id, &filename)
            .map_err(invalid_export_source)?;
        revisions.insert(
            filename,
            revision(&resolved).map_err(invalid_export_source)?,
        );
    }
    Ok((Some(directory), revisions))
}

#[cfg(feature = "desktop")]
pub fn export_html_prepared(
    mut request: ExportRequest,
    prepared: PreparedExportOperation,
) -> ApiResult<ExportOutcome> {
    let output = prepared.path().to_path_buf();
    request.output_path = Some(output.to_string_lossy().into_owned());
    let document = standalone_html(&request);
    write_export_bytes_validated(
        &output,
        document.as_bytes(),
        Some(&prepared.write_guard),
        || prepared.destination.revalidate(),
    )?;
    Ok(ExportOutcome {
        action: "saved".into(),
        path: request.output_path,
    })
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
pub async fn export_pdf_prepared(
    mut request: ExportRequest,
    window: tauri::WebviewWindow,
    prepared: PreparedExportOperation,
) -> ApiResult<ExportOutcome> {
    request.output_path = Some(prepared.path().to_string_lossy().into_owned());
    export_pdf_with_destination(
        request,
        window,
        Some(prepared.write_guard),
        Some(prepared.destination),
    )
    .await
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
async fn export_pdf_with_destination(
    request: ExportRequest,
    window: tauri::WebviewWindow,
    guard: Option<ExportWriteGuard>,
    destination: Option<DestinationSnapshot>,
) -> ApiResult<ExportOutcome> {
    if let Some(snapshot) = destination.as_ref() {
        drop(snapshot.revalidate()?);
    }
    let output_path = pdf_output_path(&request)?;
    let temporary_lease = PdfTemporaryLease::create()?;
    let temporary = temporary_lease.path().to_path_buf();
    validate_pdf_temporary(&temporary, Some(&output_path))?;
    if let Err(error) = render_pdf_to_temporary_guarded(
        &request,
        window,
        &temporary,
        Some(Arc::clone(&temporary_lease)),
    )
    .await
    {
        if let Some(snapshot) = destination.as_ref() {
            drop(snapshot.revalidate()?);
        }
        return Err(error);
    }
    let commit_temporary = temporary.clone();
    match tauri::async_runtime::spawn_blocking(move || {
        let _temporary_lease = temporary_lease;
        commit_pdf_temporary_validated(
            &request,
            &commit_temporary,
            guard.as_ref(),
            destination.as_ref(),
        )
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(ApiError::new("pdf_export_error", error.to_string()))
        }
    }
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
pub async fn render_pdf_to_temporary(
    request: &ExportRequest,
    window: tauri::WebviewWindow,
    temporary: &Path,
) -> ApiResult<()> {
    render_pdf_to_temporary_guarded(request, window, temporary, None).await
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
async fn render_pdf_to_temporary_guarded(
    request: &ExportRequest,
    window: tauri::WebviewWindow,
    temporary: &Path,
    completion_lease: Option<Arc<PdfTemporaryLease>>,
) -> ApiResult<()> {
    if request.rendered_html.trim().is_empty() {
        return Err(ApiError::new(
            "empty_export",
            "There is no rendered document to print.",
        ));
    }
    validate_pdf_temporary(temporary, None)?;
    let callback_temporary = temporary.to_path_buf();
    let print_path = temporary.to_string_lossy().into_owned();
    let landscape = request.landscape.unwrap_or(false);
    let page_size = request.page_size.as_deref().unwrap_or("A4").to_string();
    let (sender, receiver) = std::sync::mpsc::channel::<Result<(), String>>();

    window
        .with_webview(move |platform| {
            use webview2_com::{
                Microsoft::Web::WebView2::Win32::{
                    COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE,
                    COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT, ICoreWebView2_7,
                    ICoreWebView2Environment6,
                },
                PrintToPdfCompletedHandler,
            };
            use windows::core::{HSTRING, Interface};

            let completion_sender = sender.clone();
            let setup = (|| -> Result<(), String> {
                let controller = platform.controller();
                let core =
                    unsafe { controller.CoreWebView2() }.map_err(|error| error.to_string())?;
                let printable: ICoreWebView2_7 = core.cast().map_err(|error| error.to_string())?;
                let environment: ICoreWebView2Environment6 = platform
                    .environment()
                    .cast()
                    .map_err(|error| error.to_string())?;
                let settings = unsafe { environment.CreatePrintSettings() }
                    .map_err(|error| error.to_string())?;
                let (mut width, mut height) = if page_size.eq_ignore_ascii_case("letter") {
                    (8.5, 11.0)
                } else {
                    (8.27, 11.69)
                };
                if landscape {
                    std::mem::swap(&mut width, &mut height);
                }
                unsafe {
                    settings
                        .SetOrientation(if landscape {
                            COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE
                        } else {
                            COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT
                        })
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetPageWidth(width)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetPageHeight(height)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetMarginTop(0.0)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetMarginBottom(0.0)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetMarginLeft(0.0)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetMarginRight(0.0)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetShouldPrintBackgrounds(true)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetShouldPrintHeaderAndFooter(false)
                        .map_err(|error| error.to_string())?;
                }
                let handler =
                    PrintToPdfCompletedHandler::create(Box::new(move |status, success| {
                        // WebView2 can complete after InkFlow's bounded wait has
                        // timed out. Keep the private directory identity pinned
                        // until this callback actually runs so a late writer can
                        // never be redirected through a rebound path.
                        let _completion_lease = completion_lease.as_ref();
                        let result = status.map_err(|error| error.to_string()).and_then(|_| {
                            success
                                .then_some(())
                                .ok_or_else(|| "WebView2 did not create the PDF.".into())
                        });
                        send_pdf_completion(&completion_sender, result, &callback_temporary);
                        Ok(())
                    }));
                unsafe {
                    printable
                        .PrintToPdf(&HSTRING::from(print_path), &settings, &handler)
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })();
            if let Err(error) = setup {
                let _ = sender.send(Err(error));
            }
        })
        .map_err(|error| ApiError::new("pdf_export_error", error.to_string()))?;

    let completion = tauri::async_runtime::spawn_blocking(move || {
        receiver.recv_timeout(Duration::from_secs(60))
    })
    .await;

    let completed = match completion {
        Ok(Ok(completed)) => completed,
        Ok(Err(_)) => {
            let _ = std::fs::remove_file(temporary);
            return Err(ApiError::new(
                "pdf_export_timeout",
                "WebView2 PDF export timed out.",
            ));
        }
        Err(error) => {
            let _ = std::fs::remove_file(temporary);
            return Err(ApiError::new("pdf_export_error", error.to_string()));
        }
    };

    if let Err(message) = completed {
        let _ = std::fs::remove_file(temporary);
        return Err(ApiError::new("pdf_export_error", message));
    }
    Ok(())
}

#[cfg(all(test, feature = "desktop", target_os = "windows"))]
pub fn commit_pdf_temporary(
    request: &ExportRequest,
    temporary: &Path,
    guard: Option<&ExportWriteGuard>,
) -> ApiResult<ExportOutcome> {
    commit_pdf_temporary_validated(request, temporary, guard, None)
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn commit_pdf_temporary_validated(
    request: &ExportRequest,
    temporary: &Path,
    guard: Option<&ExportWriteGuard>,
    destination: Option<&DestinationSnapshot>,
) -> ApiResult<ExportOutcome> {
    let result = (|| {
        let output_path = match destination {
            Some(snapshot) => snapshot.path().to_path_buf(),
            None => pdf_output_path(request)?,
        };
        if temporary == output_path {
            return Err(ApiError::new(
                "invalid_temporary_path",
                "The PDF temporary path must differ from the destination.",
            ));
        }
        let bytes = std::fs::read(temporary)
            .map_err(|error| ApiError::io("Unable to read the generated PDF", error))?;
        write_export_bytes_validated(&output_path, &bytes, guard, || {
            destination.map(DestinationSnapshot::revalidate).transpose()
        })?;
        Ok(ExportOutcome {
            action: "saved".into(),
            path: request.output_path.clone(),
        })
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn pdf_output_path(request: &ExportRequest) -> ApiResult<PathBuf> {
    let output = request.output_path.as_deref().ok_or_else(|| {
        ApiError::new(
            "missing_output_path",
            "Choose a destination for the PDF file.",
        )
    })?;
    let output_path = PathBuf::from(output);
    let parent = output_path.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_output_path",
            "The PDF destination has no parent directory.",
        )
    })?;
    if !parent.is_dir() {
        return Err(ApiError::new(
            "missing_output_directory",
            "The PDF destination directory does not exist.",
        ));
    }
    Ok(output_path)
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn validate_pdf_temporary(temporary: &Path, output: Option<&Path>) -> ApiResult<()> {
    if !temporary.is_absolute()
        || !temporary.parent().is_some_and(Path::is_dir)
        || output.is_some_and(|output| temporary == output)
        || temporary.exists()
    {
        return Err(ApiError::new(
            "invalid_temporary_path",
            "The PDF temporary path must be a new absolute file in an existing directory.",
        ));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn write_export_bytes(
    path: &Path,
    bytes: &[u8],
    guard: Option<&ExportWriteGuard>,
) -> ApiResult<()> {
    write_export_bytes_validated(path, bytes, guard, || Ok(()))
}

pub(crate) fn write_export_bytes_validated<T, F>(
    path: &Path,
    bytes: &[u8],
    guard: Option<&ExportWriteGuard>,
    validate: F,
) -> ApiResult<()>
where
    F: FnOnce() -> ApiResult<T>,
{
    let _path_guard = lock_path_mutations()?;
    let _destination_guard = validate()?;
    let outcome = match guard {
        Some(ExportWriteGuard {
            expected_revision: Some(expected),
            ..
        }) => atomic_write_if_revision(path, bytes, Some(expected))?,
        Some(ExportWriteGuard {
            expected_revision: None,
            create_only: true,
            ..
        }) => atomic_create_if_absent(path, bytes)?,
        Some(ExportWriteGuard {
            require_existing: true,
            ..
        }) => atomic_replace_existing(path, bytes)?,
        _ => {
            atomic_write(path, bytes)?;
            AtomicWriteOutcome::Written
        }
    };
    match outcome {
        AtomicWriteOutcome::Written => Ok(()),
        AtomicWriteOutcome::Conflict(current) => Err(ApiError::new(
            "revision_conflict",
            match current {
                Some(revision) => format!(
                    "The output destination changed before it could be written (current hash {}).",
                    revision.hash
                ),
                None => "The output destination no longer exists.".into(),
            },
        )),
    }
}

#[cfg(feature = "desktop")]
fn send_pdf_completion(
    sender: &std::sync::mpsc::Sender<Result<(), String>>,
    result: Result<(), String>,
    temporary: &Path,
) {
    if sender.send(result).is_err() {
        let _ = std::fs::remove_file(temporary);
    }
}

#[cfg(all(feature = "desktop", not(target_os = "windows")))]
pub async fn export_pdf_prepared(
    _request: ExportRequest,
    _window: tauri::WebviewWindow,
    _prepared: PreparedExportOperation,
) -> ApiResult<ExportOutcome> {
    Err(ApiError::new(
        "unsupported_platform",
        "PDF export is currently available on Windows only.",
    ))
}

pub fn standalone_html(request: &ExportRequest) -> String {
    let title = escape_html(&request.title);
    let katex_css = request
        .rendered_html
        .contains("class=\"katex")
        .then(self_contained_katex_css)
        .unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>{title}</title>
  <style>{}\n{}</style>
</head>
<body><main class="inkflow-document">{}</main></body>
</html>"#,
        export_css(
            request.page_size.as_deref(),
            request.landscape.unwrap_or(false)
        ),
        katex_css,
        request.rendered_html
    )
}

fn self_contained_katex_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| {
        let mut css = include_str!("../../node_modules/katex/dist/katex.min.css").to_string();
        let fonts: &[(&str, &[u8])] = &[
            (
                "KaTeX_AMS-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_AMS-Regular.woff2"),
            ),
            (
                "KaTeX_Caligraphic-Bold",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Caligraphic-Bold.woff2"),
            ),
            (
                "KaTeX_Caligraphic-Regular",
                include_bytes!(
                    "../../node_modules/katex/dist/fonts/KaTeX_Caligraphic-Regular.woff2"
                ),
            ),
            (
                "KaTeX_Fraktur-Bold",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Fraktur-Bold.woff2"),
            ),
            (
                "KaTeX_Fraktur-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Fraktur-Regular.woff2"),
            ),
            (
                "KaTeX_Main-Bold",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Main-Bold.woff2"),
            ),
            (
                "KaTeX_Main-BoldItalic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Main-BoldItalic.woff2"),
            ),
            (
                "KaTeX_Main-Italic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Main-Italic.woff2"),
            ),
            (
                "KaTeX_Main-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Main-Regular.woff2"),
            ),
            (
                "KaTeX_Math-BoldItalic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Math-BoldItalic.woff2"),
            ),
            (
                "KaTeX_Math-Italic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Math-Italic.woff2"),
            ),
            (
                "KaTeX_SansSerif-Bold",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_SansSerif-Bold.woff2"),
            ),
            (
                "KaTeX_SansSerif-Italic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_SansSerif-Italic.woff2"),
            ),
            (
                "KaTeX_SansSerif-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_SansSerif-Regular.woff2"),
            ),
            (
                "KaTeX_Script-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Script-Regular.woff2"),
            ),
            (
                "KaTeX_Size1-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Size1-Regular.woff2"),
            ),
            (
                "KaTeX_Size2-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Size2-Regular.woff2"),
            ),
            (
                "KaTeX_Size3-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Size3-Regular.woff2"),
            ),
            (
                "KaTeX_Size4-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Size4-Regular.woff2"),
            ),
            (
                "KaTeX_Typewriter-Regular",
                include_bytes!(
                    "../../node_modules/katex/dist/fonts/KaTeX_Typewriter-Regular.woff2"
                ),
            ),
        ];
        for (name, bytes) in fonts {
            css = css.replace(
                &format!("fonts/{name}.woff2"),
                &format!("data:font/woff2;base64,{}", STANDARD.encode(bytes)),
            );
        }
        regex::Regex::new(r#",url\(fonts/[^)]*\.(?:woff|ttf)\) format\("[^"]+"\)"#)
            .expect("valid KaTeX fallback font pattern")
            .replace_all(&css, "")
            .into_owned()
    })
}

pub fn export_css(page_size: Option<&str>, landscape: bool) -> String {
    let size = match page_size.unwrap_or("A4").to_ascii_lowercase().as_str() {
        "letter" => "Letter",
        _ => "A4",
    };
    let orientation = if landscape { " landscape" } else { "" };
    format!(
        r#"
:root{{color-scheme:light;--ink:#242424;--muted:#6f6f6f;--line:#dededb}}
*{{box-sizing:border-box}}
body{{margin:0;background:#fff;color:var(--ink);font:16px/1.75 "Segoe UI", "Microsoft YaHei UI", sans-serif}}
.inkflow-document{{max-width:820px;margin:0 auto;padding:56px 44px 96px}}
h1,h2,h3,h4,h5,h6{{line-height:1.28;margin:1.7em 0 .65em;font-weight:650}}
h1{{font-size:2.1em}} h2{{font-size:1.65em;border-bottom:1px solid var(--line);padding-bottom:.25em}}
p,ul,ol,blockquote,pre,table{{margin:1em 0}}
a{{color:#356bc4;text-decoration:none}} a:hover{{text-decoration:underline}}
blockquote{{border-left:3px solid #9b9b96;margin-left:0;padding:.25em 1em;color:var(--muted)}}
code{{font-family:"Cascadia Mono",Consolas,monospace;font-size:.9em;background:#f2f2ef;border-radius:4px;padding:.12em .35em}}
pre{{background:#f6f6f3;border:1px solid #e7e7e3;border-radius:8px;padding:1em;overflow:auto}}
pre code{{background:none;padding:0}}
table{{border-collapse:collapse;width:100%}} th,td{{border:1px solid var(--line);padding:.45em .7em;text-align:left}}
img,svg{{max-width:100%;height:auto}} hr{{border:0;border-top:1px solid var(--line);margin:2em 0}}
.katex-display{{overflow-x:auto;overflow-y:hidden}}
@page{{size:{size}{orientation};margin:18mm 16mm}}
@media print{{.inkflow-document{{max-width:none;margin:0;padding:0}} pre,blockquote,table,img,svg{{break-inside:avoid}} a{{color:inherit}}}}
"#
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html_request(path: &Path) -> ExportRequest {
        ExportRequest {
            title: "Export".into(),
            rendered_html: "<p>snapshot</p>".into(),
            output_path: Some(path.to_string_lossy().into_owned()),
            page_size: Some("A4".into()),
            landscape: Some(false),
        }
    }

    #[test]
    fn escapes_the_export_title() {
        let html = standalone_html(&ExportRequest {
            title: "<unsafe>".into(),
            rendered_html: "<p>safe</p>".into(),
            output_path: None,
            page_size: None,
            landscape: None,
        });
        assert!(html.contains("&lt;unsafe&gt;"));
        assert!(!html.contains("<title><unsafe>"));
        assert!(!html.contains("data:font/woff2;base64,"));
    }

    #[test]
    #[cfg(all(feature = "desktop", target_os = "windows"))]
    fn pdf_render_lease_uses_and_cleans_a_private_directory() {
        let lease = PdfTemporaryLease::create().unwrap();
        let directory = lease.directory.clone();
        let temporary = lease.path().to_path_buf();
        let late_callback_lease = Arc::clone(&lease);
        assert_eq!(temporary.parent(), Some(directory.as_path()));
        assert!(directory.starts_with(canonical_existing(&std::env::temp_dir()).unwrap()));
        std::fs::write(&temporary, b"temporary PDF").unwrap();

        drop(lease);

        assert!(temporary.exists());
        assert!(directory.exists());
        drop(late_callback_lease);

        assert!(!temporary.exists());
        assert!(!directory.exists());
    }

    #[test]
    #[cfg(all(feature = "desktop", target_os = "windows"))]
    fn stale_pdf_directory_cleanup_is_age_name_and_activity_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let abandoned = temp.path().join(format!(
            "{PDF_TEMPORARY_DIRECTORY_PREFIX}{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&abandoned).unwrap();
        std::fs::write(abandoned.join("render.tmp.pdf"), b"private PDF").unwrap();
        let unrelated = temp.path().join("InkFlow-pdf-not-an-inkflow-uuid");
        std::fs::create_dir(&unrelated).unwrap();
        std::fs::write(unrelated.join("keep.txt"), b"keep").unwrap();

        let now = SystemTime::now();
        cleanup_stale_pdf_temporary_directories_at(temp.path(), now);
        assert!(abandoned.exists());

        cleanup_stale_pdf_temporary_directories_at(
            temp.path(),
            now + STALE_PDF_TEMPORARY_DIRECTORY_AGE + Duration::from_secs(1),
        );

        assert!(!abandoned.exists());
        assert!(unrelated.exists());

        let active = PdfTemporaryLease::create().unwrap();
        let active_directory = active.directory.clone();
        cleanup_stale_pdf_temporary_directories_at(
            active_directory.parent().unwrap(),
            now + STALE_PDF_TEMPORARY_DIRECTORY_AGE + Duration::from_secs(1),
        );
        assert!(active_directory.exists());
        drop(active);
        assert!(!active_directory.exists());
    }

    #[test]
    fn embeds_katex_assets_when_math_is_present() {
        let html = standalone_html(&ExportRequest {
            title: "Math".into(),
            rendered_html: "<span class=\"katex\">formula</span>".into(),
            output_path: None,
            page_size: None,
            landscape: None,
        });

        assert!(html.contains("data:font/woff2;base64,"));
        assert!(!html.contains("url(fonts/"));
    }

    #[test]
    fn guarded_export_refuses_to_replace_a_concurrently_created_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.html");
        std::fs::write(&path, b"external").unwrap();

        let error = write_export_bytes(
            &path,
            b"inkflow",
            Some(&ExportWriteGuard {
                expected_revision: None,
                create_only: true,
                require_existing: false,
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
    }

    #[test]
    fn guarded_export_refuses_a_stale_revision() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.html");
        std::fs::write(&path, b"first").unwrap();
        let expected = crate::fileio::revision(&path).unwrap();
        std::fs::write(&path, b"external").unwrap();

        let error = write_export_bytes(
            &path,
            b"inkflow",
            Some(&ExportWriteGuard {
                expected_revision: Some(expected),
                create_only: false,
                require_existing: true,
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn prepared_export_refuses_a_modified_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("prepared.html");
        std::fs::write(&path, b"selected version").unwrap();
        let store = ExportDestinationStore::new();
        let prepared = store.prepare(&path).unwrap();
        std::fs::write(&path, b"external version").unwrap();

        let operation = store.take(&prepared.token, Some(&prepared.path)).unwrap();
        let error = export_html_prepared(html_request(&path), operation).unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(std::fs::read(path).unwrap(), b"external version");
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn prepared_export_refuses_a_concurrently_created_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("new.html");
        let store = ExportDestinationStore::new();
        let prepared = store.prepare(&path).unwrap();
        std::fs::write(&path, b"external version").unwrap();

        let operation = store.take(&prepared.token, Some(&prepared.path)).unwrap();
        let error = export_html_prepared(html_request(&path), operation).unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(std::fs::read(path).unwrap(), b"external version");
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn prepared_export_refuses_a_moved_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected.html");
        let moved = temp.path().join("moved.html");
        std::fs::write(&path, b"selected version").unwrap();
        let store = ExportDestinationStore::new();
        let prepared = store.prepare(&path).unwrap();
        std::fs::rename(&path, &moved).unwrap();

        let operation = store.take(&prepared.token, Some(&prepared.path)).unwrap();
        let error = export_html_prepared(html_request(&path), operation).unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert!(!path.exists());
        assert_eq!(std::fs::read(moved).unwrap(), b"selected version");
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn prepared_export_refuses_a_replaced_parent_directory() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("destination");
        let displaced = temp.path().join("destination-old");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("export.html");
        let store = ExportDestinationStore::new();
        let prepared = store.prepare(&path).unwrap();
        std::fs::rename(&parent, &displaced).unwrap();
        std::fs::create_dir(&parent).unwrap();

        let operation = store.take(&prepared.token, Some(&prepared.path)).unwrap();
        let error = export_html_prepared(html_request(&path), operation).unwrap_err();

        assert_eq!(error.code, "path_changed");
        assert!(!path.exists());
        assert!(!displaced.join("export.html").exists());
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn prepared_export_reports_a_missing_parent_as_a_path_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("destination");
        let displaced = temp.path().join("destination-old");
        std::fs::create_dir(&parent).unwrap();
        let path = parent.join("export.html");
        std::fs::write(&path, b"selected version").unwrap();
        let store = ExportDestinationStore::new();
        let prepared = store.prepare(&path).unwrap();
        std::fs::rename(&parent, &displaced).unwrap();

        let operation = store.take(&prepared.token, Some(&prepared.path)).unwrap();
        let error = export_html_prepared(html_request(&path), operation).unwrap_err();

        assert_eq!(error.code, "path_changed");
        assert!(!path.exists());
        assert_eq!(
            std::fs::read(displaced.join("export.html")).unwrap(),
            b"selected version"
        );
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn export_destination_tokens_are_single_use_and_cancellable() {
        let temp = tempfile::tempdir().unwrap();
        let store = ExportDestinationStore::new();
        let path = temp.path().join("single-use.html");
        let prepared = store.prepare(&path).unwrap();
        let _operation = store.take(&prepared.token, Some(&prepared.path)).unwrap();
        assert_eq!(
            store
                .take(&prepared.token, Some(&prepared.path))
                .unwrap_err()
                .code,
            "invalid_export_token"
        );

        let cancelled = store.prepare(&path).unwrap();
        store.cancel(&cancelled.token);
        assert_eq!(
            store
                .take(&cancelled.token, Some(&cancelled.path))
                .unwrap_err()
                .code,
            "invalid_export_token"
        );
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn export_source_tokens_keep_an_immutable_reusable_scope() {
        let temp = tempfile::tempdir().unwrap();
        let store = ExportDestinationStore::new();
        let document_path = temp.path().join("document.md");
        let workspace_root = temp.path().join("workspace");
        let recovery_root = temp.path().join("recovery");
        std::fs::create_dir(&workspace_root).unwrap();
        std::fs::create_dir(&recovery_root).unwrap();
        let prepared = store
            .prepare_source(
                "document-a".into(),
                Some(document_path.clone()),
                Some(workspace_root.clone()),
                recovery_root,
            )
            .unwrap();

        for _ in 0..2 {
            let scope = store.source(&prepared.token).unwrap();
            drop(scope.guard_resource("images/a.png").unwrap());
            assert_eq!(scope.document_id, "document-a");
            assert_eq!(
                scope.document_path.as_deref(),
                Some(document_path.as_path())
            );
            assert_eq!(
                scope.workspace_root.as_deref(),
                Some(workspace_root.as_path())
            );
        }

        store.cancel_source(&prepared.token);
        assert_eq!(
            store.source(&prepared.token).unwrap_err().code,
            "invalid_export_source"
        );
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn export_source_tokens_remain_valid_until_release_and_are_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let store = ExportDestinationStore::with_policy(Duration::ZERO, 1);
        let prepared = store
            .prepare_source("document-a".into(), None, None, temp.path().to_path_buf())
            .unwrap();

        assert!(store.source(&prepared.token).is_ok());
        assert_eq!(
            store
                .prepare_source("document-b".into(), None, None, temp.path().to_path_buf())
                .unwrap_err()
                .code,
            "too_many_export_sources"
        );

        store.cancel_source(&prepared.token);
        assert!(
            store
                .prepare_source("document-b".into(), None, None, temp.path().to_path_buf())
                .is_ok()
        );
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn export_source_scope_rejects_a_replaced_document_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let displaced = temp.path().join("source-old");
        let recovery = temp.path().join("recovery");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&recovery).unwrap();
        let store = ExportDestinationStore::new();
        let prepared = store
            .prepare_source(
                "document-a".into(),
                Some(source.join("document.md")),
                None,
                recovery,
            )
            .unwrap();
        let scope = store.source(&prepared.token).unwrap();
        drop(scope.guard_resource("images/a.png").unwrap());

        std::fs::rename(&source, &displaced).unwrap();
        std::fs::create_dir(&source).unwrap();

        let error = match scope.guard_resource("images/a.png") {
            Ok(_) => panic!("a rebound export source directory must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.code, "invalid_export_source");
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn export_source_rejects_a_pending_asset_removed_after_preparation() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("recovery");
        let pending = recovery.join("assets").join("document-a");
        std::fs::create_dir_all(&pending).unwrap();
        let image = pending.join("image.png");
        std::fs::write(&image, b"snapshot image").unwrap();
        // Keep the directory alive so the file revision snapshot, rather than
        // only the directory identity, has to detect the migration/removal.
        std::fs::write(pending.join("unreferenced.png"), b"other image").unwrap();
        let store = ExportDestinationStore::new();
        let prepared = store
            .prepare_source("document-a".into(), None, None, recovery.clone())
            .unwrap();
        let scope = store.source(&prepared.token).unwrap();
        assert!(
            scope
                .load_pending_asset("inkflow-asset://image.png")
                .unwrap()
                .is_some()
        );

        std::fs::remove_file(&image).unwrap();

        let error = scope
            .load_pending_asset("inkflow-asset://image.png")
            .unwrap_err();
        assert_eq!(error.code, "invalid_export_source");
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn export_source_treats_an_asset_missing_at_preparation_as_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("recovery");
        std::fs::create_dir(&recovery).unwrap();
        let store = ExportDestinationStore::new();
        let prepared = store
            .prepare_source("document-a".into(), None, None, recovery.clone())
            .unwrap();
        let scope = store.source(&prepared.token).unwrap();

        let error = scope
            .load_pending_asset("inkflow-asset://missing.png")
            .unwrap_err();
        assert_eq!(error.code, "resource_not_found");

        let pending = recovery.join("assets").join("document-a");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::write(pending.join("missing.png"), b"created too late").unwrap();
        let error = scope
            .load_pending_asset("inkflow-asset://missing.png")
            .unwrap_err();
        assert_eq!(error.code, "resource_not_found");
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn export_source_does_not_lock_a_replaced_recovery_directory() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("recovery");
        let displaced = temp.path().join("recovery-old");
        let pending = recovery.join("assets").join("document-a");
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::write(pending.join("image.png"), b"snapshot image").unwrap();
        let store = ExportDestinationStore::new();
        let prepared = store
            .prepare_source("document-a".into(), None, None, recovery.clone())
            .unwrap();
        let scope = store.source(&prepared.token).unwrap();

        std::fs::rename(&recovery, &displaced).unwrap();
        std::fs::create_dir(&recovery).unwrap();

        let error = scope
            .load_pending_asset("inkflow-asset://image.png")
            .unwrap_err();
        assert_eq!(error.code, "invalid_export_source");
        assert!(
            !recovery.join(".recovery.lock").exists(),
            "a replacement Recovery directory must not be mutated before identity validation"
        );
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn export_destination_tokens_reject_mismatch_expiry_and_overflow() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("selected.html");
        let mismatch_store = ExportDestinationStore::new();
        let mismatch = mismatch_store.prepare(&path).unwrap();
        let other = temp
            .path()
            .join("other.html")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            mismatch_store
                .take(&mismatch.token, Some(&other))
                .unwrap_err()
                .code,
            "invalid_export_token"
        );

        let expired_store = ExportDestinationStore::with_policy(Duration::ZERO, 16);
        let expired = expired_store.prepare(&path).unwrap();
        assert_eq!(
            expired_store
                .take(&expired.token, Some(&expired.path))
                .unwrap_err()
                .code,
            "expired_export_token"
        );

        let full_store = ExportDestinationStore::with_policy(Duration::from_secs(60), 1);
        full_store.prepare(&path).unwrap();
        assert_eq!(
            full_store
                .prepare(&temp.path().join("overflow.html"))
                .unwrap_err()
                .code,
            "too_many_export_destinations"
        );
    }

    #[test]
    fn forced_export_does_not_recreate_a_moved_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.html");
        let moved = temp.path().join("moved.html");
        std::fs::write(&path, b"original").unwrap();
        std::fs::rename(&path, &moved).unwrap();

        let error = write_export_bytes(
            &path,
            b"inkflow",
            Some(&ExportWriteGuard {
                expected_revision: None,
                create_only: false,
                require_existing: true,
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert!(!path.exists());
        assert_eq!(std::fs::read(moved).unwrap(), b"original");
    }

    #[test]
    fn forced_export_still_replaces_an_existing_changed_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.html");
        std::fs::write(&path, b"changed externally").unwrap();

        write_export_bytes(
            &path,
            b"inkflow",
            Some(&ExportWriteGuard {
                expected_revision: None,
                create_only: false,
                require_existing: true,
            }),
        )
        .unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"inkflow");
    }

    #[test]
    #[cfg(all(feature = "desktop", target_os = "windows"))]
    fn commits_a_private_pdf_and_removes_the_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("export.pdf");
        let temporary = temp.path().join("private.tmp.pdf");
        std::fs::write(&temporary, b"generated PDF").unwrap();
        let request = ExportRequest {
            title: "PDF".into(),
            rendered_html: "<p>PDF</p>".into(),
            output_path: Some(output.to_string_lossy().into_owned()),
            page_size: Some("A4".into()),
            landscape: Some(false),
        };

        let outcome = commit_pdf_temporary(
            &request,
            &temporary,
            Some(&ExportWriteGuard {
                expected_revision: None,
                create_only: true,
                require_existing: false,
            }),
        )
        .unwrap();

        assert_eq!(outcome.path.as_deref(), request.output_path.as_deref());
        assert_eq!(std::fs::read(output).unwrap(), b"generated PDF");
        assert!(!temporary.exists());
    }

    #[test]
    #[cfg(all(feature = "desktop", target_os = "windows"))]
    fn prepared_pdf_reports_a_missing_parent_as_a_path_conflict() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("destination");
        let displaced = temp.path().join("destination-old");
        std::fs::create_dir(&parent).unwrap();
        let output = parent.join("export.pdf");
        let temporary = temp.path().join("generated.tmp.pdf");
        std::fs::write(&temporary, b"generated PDF").unwrap();

        let store = ExportDestinationStore::new();
        let prepared = store.prepare(&output).unwrap();
        let operation = store.take(&prepared.token, Some(&prepared.path)).unwrap();
        std::fs::rename(&parent, &displaced).unwrap();
        let request = ExportRequest {
            title: "PDF".into(),
            rendered_html: "<p>snapshot</p>".into(),
            output_path: Some(prepared.path),
            page_size: Some("A4".into()),
            landscape: Some(false),
        };

        let error = commit_pdf_temporary_validated(
            &request,
            &temporary,
            Some(&operation.write_guard),
            Some(&operation.destination),
        )
        .unwrap_err();

        assert_eq!(error.code, "path_changed");
        assert!(!output.exists());
        assert!(!displaced.join("export.pdf").exists());
        assert!(!temporary.exists());
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn late_pdf_completion_removes_the_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("late.pdf");
        std::fs::write(&path, b"late PDF").unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(receiver);

        send_pdf_completion(&sender, Ok(()), &path);

        assert!(!path.exists());
    }
}
