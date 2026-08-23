use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};
use regex::Regex;

use crate::{
    data_lock::{DataLock, PathMutationLock},
    error::{ApiError, ApiResult},
    fileio::{
        AtomicWriteOutcome, atomic_create_if_absent, canonical_existing,
        is_symbolic_link_or_junction,
    },
    model::{WriteAssetRequest, WriteAssetResult},
};

#[cfg(test)]
use crate::data_lock::lock_path_mutations;

const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_BASE64_IMAGE_BYTES: usize = (MAX_IMAGE_BYTES as usize).div_ceil(3) * 4;

#[cfg(test)]
pub fn write_asset(recovery_dir: &Path, request: WriteAssetRequest) -> ApiResult<WriteAssetResult> {
    let path_lock = lock_path_mutations()?;
    write_asset_locked(recovery_dir, request, &path_lock)
}

pub(crate) fn write_asset_locked(
    recovery_dir: &Path,
    request: WriteAssetRequest,
    _path_lock: &PathMutationLock,
) -> ApiResult<WriteAssetResult> {
    prepare_asset(recovery_dir, request, true)
}

#[cfg(feature = "cli")]
pub fn preview_asset(
    recovery_dir: &Path,
    request: WriteAssetRequest,
) -> ApiResult<WriteAssetResult> {
    prepare_asset(recovery_dir, request, false)
}

fn prepare_asset(
    recovery_dir: &Path,
    request: WriteAssetRequest,
    write: bool,
) -> ApiResult<WriteAssetResult> {
    let (bytes, extension) = asset_bytes_and_extension(&request)?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let hash_prefix = &hash[..16];

    // Saved-document assets share the Save As namespace lock. Without this, a
    // concurrent asset insertion could reuse a file owned by a failing Save As
    // transaction and then watch its newly referenced file get rolled back.
    let _saved_asset_lock = if write {
        request
            .document_path
            .as_deref()
            .map(Path::new)
            .map(lock_save_as_destination)
            .transpose()?
    } else {
        None
    };

    // Pending assets share the recovery lock with migration and cleanup. This
    // prevents a desktop save from deleting an asset that a CLI process has
    // just added to the same unsaved document.
    let _pending_lock = if write && request.document_path.is_none() {
        Some(lock_pending_assets(recovery_dir)?)
    } else {
        None
    };

    let (directory, markdown_prefix, pending) = match request.document_path.as_deref() {
        Some(document_path) => {
            let document = PathBuf::from(document_path);
            let directory = document_asset_directory(&document)?;
            let folder = directory
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
                .unwrap_or_else(|| "document.assets".into());
            (directory, folder, false)
        }
        None => (
            recovery_dir
                .join("assets")
                .join(safe_component(&request.document_id)?),
            "inkflow-asset:".into(),
            true,
        ),
    };
    if write {
        fs::create_dir_all(&directory)
            .map_err(|error| ApiError::io("Unable to create the asset directory", error))?;
    }

    if directory.is_dir()
        && let Some(existing) = find_existing_asset(&directory, &hash, hash_prefix, &extension)?
    {
        return Ok(asset_result(existing, &markdown_prefix, pending));
    }

    // Content-addressed names make a preview a stable plan: a later write of
    // the same bytes resolves to exactly the path reported by --dry-run.
    // The full hash also avoids turning a truncated-hash collision into an
    // overwrite of an unrelated image.
    let filename = format!("image-{hash}.{extension}");
    let path = directory.join(filename);
    if write {
        match atomic_create_if_absent(&path, &bytes)? {
            AtomicWriteOutcome::Written => {}
            AtomicWriteOutcome::Conflict(_) => validate_asset_path_contents(&path, &bytes)?,
        }
    } else if path.exists() {
        validate_asset_path_contents(&path, &bytes)?;
    }
    Ok(asset_result(path, &markdown_prefix, pending))
}

fn validate_asset_path_contents(path: &Path, expected: &[u8]) -> ApiResult<()> {
    let existing = fs::read(path)
        .map_err(|error| ApiError::io("Unable to inspect an existing asset", error))?;
    if existing != expected {
        return Err(ApiError::new(
            "asset_hash_collision",
            "The content-addressed asset path is occupied by different data.",
        ));
    }
    Ok(())
}

pub fn document_asset_directory(document_path: &Path) -> ApiResult<PathBuf> {
    let parent = document_path
        .parent()
        .ok_or_else(|| ApiError::new("invalid_path", "The document has no parent directory."))?;
    let stem = document_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("document");
    Ok(parent.join(format!("{stem}.assets")))
}

pub struct PendingAssetsLock {
    recovery_dir: PathBuf,
    _lock: DataLock,
}

pub fn lock_pending_assets(recovery_dir: &Path) -> ApiResult<PendingAssetsLock> {
    Ok(PendingAssetsLock {
        recovery_dir: recovery_dir.to_path_buf(),
        _lock: DataLock::acquire(&recovery_dir.join(".recovery.lock"))?,
    })
}

pub fn has_pending_asset_references(content: &str) -> bool {
    !pending_asset_filenames(content).is_empty()
}

/// Serializes Save As transactions that share one destination asset namespace
/// across InkFlow processes without writing lock metadata into the workspace.
pub fn lock_save_as_destination(destination: &Path) -> ApiResult<DataLock> {
    DataLock::acquire(&save_as_lock_path(destination)?)
}

fn save_as_lock_path(destination: &Path) -> ApiResult<PathBuf> {
    // Different document extensions can produce the same `<stem>.assets`
    // directory, so the asset namespace—not the document filename—is the
    // transaction identity.
    let asset_directory = document_asset_directory(destination)?;
    let normalized = if asset_directory.exists() {
        canonical_existing(&asset_directory)?
    } else {
        let parent = asset_directory.parent().ok_or_else(|| {
            ApiError::new(
                "invalid_path",
                "The Save As asset destination has no parent directory.",
            )
        })?;
        canonical_existing(parent)?.join(asset_directory.file_name().ok_or_else(|| {
            ApiError::new(
                "invalid_path",
                "The Save As asset destination has no directory name.",
            )
        })?)
    };
    let identity = normalized
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase();
    let key = blake3::hash(identity.as_bytes()).to_hex().to_string();
    let lock_path = std::env::temp_dir()
        .join("InkFlow")
        .join("Locks")
        .join(format!("save-as-{}.lock", &key[..32]));
    Ok(lock_path)
}

pub fn migrate_pending_assets(
    lock: &PendingAssetsLock,
    document_id: &str,
    document_path: &Path,
    content: &str,
) -> ApiResult<String> {
    let document_id = safe_component(document_id)?;
    let referenced_assets = pending_asset_filenames(content);
    if referenced_assets.is_empty() {
        return Ok(content.to_string());
    }
    let recovery_dir = &lock.recovery_dir;
    let pending = recovery_dir.join("assets").join(document_id);
    if !pending.exists() {
        return Ok(content.to_string());
    }

    let recovery_root = canonical_existing(recovery_dir)?;
    let resolved_pending = canonical_existing(&pending)?;
    if !resolved_pending.starts_with(&recovery_root) {
        return Err(ApiError::new(
            "invalid_asset_path",
            "The pending asset directory is outside the recovery area.",
        ));
    }

    let destination = document_asset_directory(document_path)?;
    let folder_name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document.assets")
        .to_string();
    fs::create_dir_all(&destination)
        .map_err(|error| ApiError::io("Unable to create the document asset directory", error))?;

    let mut replacements = HashMap::new();
    for item in fs::read_dir(&resolved_pending)
        .map_err(|error| ApiError::io("Unable to scan pending assets", error))?
    {
        let source = item
            .map_err(|error| ApiError::io("Unable to inspect a pending asset", error))?
            .path();
        if !source.is_file() {
            continue;
        }
        let filename = source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ApiError::new("invalid_asset_path", "Asset name is not valid Unicode.")
            })?;
        if !referenced_assets.contains(filename) {
            continue;
        }
        let mut target = destination.join(filename);
        if target.exists() {
            let source_bytes = fs::read(&source)
                .map_err(|error| ApiError::io("Unable to inspect a pending asset", error))?;
            let target_bytes = fs::read(&target)
                .map_err(|error| ApiError::io("Unable to inspect a destination asset", error))?;
            if source_bytes != target_bytes {
                let hash = blake3::hash(&source_bytes).to_hex();
                let stem = source
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("image");
                let extension = source
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("png");
                target = destination.join(format!("{stem}-{}.{extension}", &hash[..16]));
            }
        }
        if !target.exists() {
            fs::copy(&source, &target)
                .map_err(|error| ApiError::io("Unable to migrate a pending asset", error))?;
        }
        let target_filename = target
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ApiError::new("invalid_asset_path", "Asset name is not valid Unicode.")
            })?;
        replacements.insert(
            format!("inkflow-asset://{filename}"),
            encode_generated_resource_path(&format!("{folder_name}/{target_filename}")),
        );
    }
    Ok(rewrite_image_destinations(content, &replacements))
}

pub fn cleanup_pending_assets(
    lock: &PendingAssetsLock,
    document_id: &str,
    committed_content: &str,
) -> ApiResult<()> {
    let document_id = safe_component(document_id)?;
    let referenced_assets = pending_asset_filenames(committed_content);
    if referenced_assets.is_empty() {
        return Ok(());
    }
    let recovery_dir = &lock.recovery_dir;
    let pending = recovery_dir.join("assets").join(document_id);
    if !pending.exists() {
        return Ok(());
    }
    let assets_root = recovery_dir.join("assets");
    let resolved_assets = canonical_existing(&assets_root)?;
    let resolved_pending = canonical_existing(&pending)?;
    if resolved_pending == resolved_assets || !resolved_pending.starts_with(&resolved_assets) {
        return Err(ApiError::new(
            "invalid_asset_path",
            "The pending asset directory is outside its document scope.",
        ));
    }
    for filename in referenced_assets {
        let candidate = resolved_pending.join(filename);
        if candidate.is_file() {
            fs::remove_file(&candidate)
                .map_err(|error| ApiError::io("Unable to clean a pending asset", error))?;
        }
    }
    if fs::read_dir(&resolved_pending)
        .map_err(|error| ApiError::io("Unable to inspect pending assets", error))?
        .next()
        .is_none()
    {
        fs::remove_dir(&resolved_pending)
            .map_err(|error| ApiError::io("Unable to clean the pending asset directory", error))?;
    }
    Ok(())
}

fn pending_asset_filenames(content: &str) -> HashSet<String> {
    if !content.contains("inkflow-asset://") {
        return HashSet::new();
    }
    collect_image_destinations(content)
        .into_iter()
        .filter_map(|destination| {
            destination
                .path
                .strip_prefix("inkflow-asset://")
                .filter(|filename| safe_component(filename).is_ok())
                .map(str::to_string)
        })
        .collect()
}

#[cfg(test)]
pub fn copy_referenced_assets_for_save_as(
    source_document: &Path,
    destination_document: &Path,
    content: &str,
    workspace_root: Option<&Path>,
) -> ApiResult<String> {
    Ok(copy_referenced_assets_for_save_as_tracked(
        source_document,
        destination_document,
        content,
        workspace_root,
    )?
    .commit())
}

/// Owns any files created while preparing a Save As operation. Unless the
/// caller commits the guard after the document itself was written, dropping it
/// removes those files again. This keeps a failed guarded save from leaving a
/// destination asset directory behind.
#[must_use = "the copied assets must be committed after the document write succeeds"]
pub struct ReferencedAssetCopy {
    content: String,
    created_files: Vec<(PathBuf, blake3::Hash)>,
    created_directory: Option<PathBuf>,
    committed: bool,
}

impl ReferencedAssetCopy {
    fn new() -> Self {
        Self {
            content: String::new(),
            created_files: Vec::new(),
            created_directory: None,
            committed: false,
        }
    }

    pub(crate) fn content(&self) -> &str {
        &self.content
    }

    #[cfg(feature = "cli")]
    pub(crate) fn created_any(&self) -> bool {
        !self.created_files.is_empty()
    }

    pub fn commit(mut self) -> String {
        self.committed = true;
        std::mem::take(&mut self.content)
    }

    fn ensure_destination_directory(&mut self, destination: &Path) -> ApiResult<()> {
        if destination.is_dir() {
            return Ok(());
        }
        match fs::create_dir(destination) {
            Ok(()) => {
                self.created_directory = Some(destination.to_path_buf());
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(ApiError::io(
                "Unable to create the destination asset directory",
                error,
            )),
        }
    }

    fn track_file(&mut self, path: PathBuf, bytes: &[u8]) {
        self.created_files.push((path, blake3::hash(bytes)));
    }
}

impl Drop for ReferencedAssetCopy {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for (path, expected_hash) in self.created_files.iter().rev() {
            let unchanged = fs::read(path)
                .map(|bytes| blake3::hash(&bytes) == *expected_hash)
                .unwrap_or(false);
            if unchanged {
                let _ = fs::remove_file(path);
            }
        }
        if let Some(directory) = self.created_directory.as_ref() {
            // remove_dir intentionally succeeds only when the directory is
            // empty, so files created by another process are never removed.
            let _ = fs::remove_dir(directory);
        }
    }
}

pub fn copy_referenced_assets_for_save_as_tracked(
    source_document: &Path,
    destination_document: &Path,
    content: &str,
    workspace_root: Option<&Path>,
) -> ApiResult<ReferencedAssetCopy> {
    let mut copy = ReferencedAssetCopy::new();
    let (content, _) = prepare_referenced_assets_for_save_as(
        source_document,
        destination_document,
        content,
        workspace_root,
        Some(&mut copy),
    )?;
    copy.content = content;
    Ok(copy)
}

#[cfg(feature = "cli")]
pub(crate) struct ReferencedAssetPreview {
    content: String,
    requires_copy: bool,
}

#[cfg(feature = "cli")]
impl ReferencedAssetPreview {
    pub(crate) fn content(&self) -> &str {
        &self.content
    }

    pub(crate) fn requires_copy(&self) -> bool {
        self.requires_copy
    }
}

#[cfg(feature = "cli")]
pub(crate) fn preview_referenced_assets_for_save_as_plan(
    source_document: &Path,
    destination_document: &Path,
    content: &str,
    workspace_root: Option<&Path>,
) -> ApiResult<ReferencedAssetPreview> {
    let (content, requires_copy) = prepare_referenced_assets_for_save_as(
        source_document,
        destination_document,
        content,
        workspace_root,
        None,
    )?;
    Ok(ReferencedAssetPreview {
        content,
        requires_copy,
    })
}

#[cfg(test)]
pub fn preview_referenced_assets_for_save_as(
    source_document: &Path,
    destination_document: &Path,
    content: &str,
    workspace_root: Option<&Path>,
) -> ApiResult<String> {
    Ok(prepare_referenced_assets_for_save_as(
        source_document,
        destination_document,
        content,
        workspace_root,
        None,
    )?
    .0)
}

fn prepare_referenced_assets_for_save_as(
    source_document: &Path,
    destination_document: &Path,
    content: &str,
    workspace_root: Option<&Path>,
    mut copy: Option<&mut ReferencedAssetCopy>,
) -> ApiResult<(String, bool)> {
    let source_parent = source_document.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_path",
            "The source document has no parent directory.",
        )
    })?;
    // Saving the editor buffer must remain possible after the original Markdown
    // file was deleted. In that case its parent can still provide local assets,
    // but the source file itself must not be required to exist.
    let Ok(document_scope) = canonical_existing(source_parent) else {
        return Ok((content.to_string(), false));
    };
    let resolved_source_document = source_document
        .file_name()
        .map(|name| document_scope.join(name))
        .unwrap_or_else(|| source_document.to_path_buf());
    let source_scope = workspace_root
        .and_then(|root| canonical_existing(root).ok())
        .filter(|root| resolved_source_document.starts_with(root))
        .unwrap_or(document_scope);
    let destination = document_asset_directory(destination_document)?;
    let asset_folder = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document.assets")
        .to_string();
    let mut seen_paths = HashSet::new();
    let paths: Vec<String> = collect_image_destinations(content)
        .into_iter()
        .map(|destination| destination.path)
        .filter(|path| seen_paths.insert(path.clone()))
        .collect();

    let mut replacements = HashMap::new();
    let mut reserved_targets = HashMap::new();
    let mut requires_copy = false;
    for markdown_path in paths {
        if is_non_local_resource(&markdown_path) {
            continue;
        }
        let Some(relative_paths) = safe_relative_resource_paths(&markdown_path) else {
            continue;
        };
        let mut source = None;
        for relative in relative_paths {
            let Some(candidate) = resolve_local_resource_path(
                &source_parent.join(relative),
                workspace_root.is_some(),
            )?
            else {
                continue;
            };
            if candidate.starts_with(&source_scope)
                && candidate.is_file()
                && is_image_path(&candidate)
            {
                source = Some(candidate);
                break;
            }
        }
        let Some(source) = source else {
            continue;
        };
        let bytes = fs::read(&source)
            .map_err(|error| ApiError::io("Unable to read a referenced image", error))?;
        let (target, target_requires_copy) = prepare_save_as_asset_target(
            &source,
            &destination,
            &bytes,
            &mut reserved_targets,
            &mut copy,
        )?;
        requires_copy |= target_requires_copy;
        let new_path = encode_generated_resource_path(&format!(
            "{asset_folder}/{}",
            target.file_name().unwrap_or_default().to_string_lossy()
        ));
        replacements.insert(markdown_path, new_path);
    }
    Ok((
        rewrite_image_destinations(content, &replacements),
        requires_copy,
    ))
}

fn prepare_save_as_asset_target(
    source: &Path,
    destination: &Path,
    bytes: &[u8],
    reserved_targets: &mut HashMap<PathBuf, blake3::Hash>,
    copy: &mut Option<&mut ReferencedAssetCopy>,
) -> ApiResult<(PathBuf, bool)> {
    let filename = source
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("image.png");
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("image");
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("png");
    let content_hash = blake3::hash(bytes);
    let hash = content_hash.to_hex().to_string();

    for collision in 0..1_000_u16 {
        let target = match collision {
            0 => destination.join(filename),
            1 => destination.join(format!("{stem}-{}.{extension}", &hash[..8])),
            value => destination.join(format!("{stem}-{}-{value}.{extension}", &hash[..8])),
        };
        if let Some(reserved_hash) = reserved_targets.get(&target) {
            if reserved_hash == &content_hash {
                return Ok((target, false));
            }
            continue;
        }
        if target.exists() {
            let existing = fs::read(&target)
                .map_err(|error| ApiError::io("Unable to inspect a destination asset", error))?;
            reserved_targets.insert(target.clone(), blake3::hash(&existing));
            if existing == bytes {
                return Ok((target, false));
            }
            continue;
        }
        let Some(transaction) = copy.as_deref_mut() else {
            reserved_targets.insert(target.clone(), content_hash);
            return Ok((target, true));
        };
        transaction.ensure_destination_directory(destination)?;
        match atomic_create_if_absent(&target, bytes)? {
            AtomicWriteOutcome::Written => {
                transaction.track_file(target.clone(), bytes);
                reserved_targets.insert(target.clone(), content_hash);
                return Ok((target, true));
            }
            AtomicWriteOutcome::Conflict(_) => {
                let existing = fs::read(&target).map_err(|error| {
                    ApiError::io("Unable to inspect a concurrently created asset", error)
                })?;
                reserved_targets.insert(target.clone(), blake3::hash(&existing));
                if existing == bytes {
                    return Ok((target, false));
                }
            }
        }
    }
    Err(ApiError::new(
        "asset_name_conflict",
        "Unable to choose a safe destination name for the referenced image.",
    ))
}

pub fn read_resource(
    document_path: &Path,
    workspace_root: Option<&Path>,
    resource: &str,
) -> ApiResult<String> {
    if is_remote_resource(resource) {
        return Err(ApiError::new(
            "remote_resource_blocked",
            "Remote images are blocked until the document is trusted.",
        ));
    }
    let resource_paths = safe_relative_resource_paths(resource).ok_or_else(|| {
        ApiError::new(
            "resource_outside_scope",
            "Absolute paths and non-file resources are not loaded inline.",
        )
    })?;
    let document_parent = document_path
        .parent()
        .ok_or_else(|| ApiError::new("invalid_path", "The document has no parent directory."))?;
    let parent = canonical_existing(document_parent)?;
    let workspace_scope = workspace_root
        .and_then(|root| canonical_existing(root).ok())
        .filter(|root| document_path.starts_with(root));
    let mut resolved = None;
    for resource_path in resource_paths {
        let Some(candidate) = resolve_local_resource_path(
            &document_parent.join(resource_path),
            workspace_root.is_some(),
        )?
        else {
            continue;
        };
        let allowed_by_document = candidate.starts_with(&parent);
        let allowed_by_workspace = workspace_scope
            .as_ref()
            .is_some_and(|root| candidate.starts_with(root));
        if (allowed_by_document || allowed_by_workspace)
            && candidate.is_file()
            && is_image_path(&candidate)
        {
            resolved = Some(candidate);
            break;
        }
    }
    let resolved = resolved.ok_or_else(|| {
        ApiError::new(
            "resource_outside_scope",
            "The image is outside the active document scope.",
        )
    })?;
    let metadata = fs::metadata(&resolved)
        .map_err(|error| ApiError::io("Unable to inspect the image", error))?;
    if metadata.len() > 50 * 1024 * 1024 {
        return Err(ApiError::new(
            "resource_too_large",
            "Images larger than 50 MB are not loaded inline.",
        ));
    }
    let bytes =
        fs::read(&resolved).map_err(|error| ApiError::io("Unable to read the image", error))?;
    let mime = mime_for_extension(
        resolved
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default(),
    );
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

fn resolve_local_resource_path(
    candidate: &Path,
    reject_reparse_points: bool,
) -> ApiResult<Option<PathBuf>> {
    if reject_reparse_points {
        reject_existing_reparse_components(candidate)?;
    }
    Ok(canonical_existing(candidate).ok())
}

fn reject_existing_reparse_components(candidate: &Path) -> ApiResult<()> {
    let mut current = PathBuf::new();
    for component in candidate.components() {
        current.push(component);
        if !current.exists() {
            break;
        }
        if is_symbolic_link_or_junction(&current)? {
            return Err(ApiError::new(
                "reparse_point_blocked",
                "Symbolic links and directory junctions are not followed in a scoped workspace.",
            ));
        }
    }
    Ok(())
}

fn is_remote_resource(resource: &str) -> bool {
    let value = resource
        .trim()
        .chars()
        .filter(|character| !matches!(character, '\t' | '\n' | '\r'))
        .map(|character| if character == '\\' { '/' } else { character })
        .collect::<String>()
        .to_ascii_lowercase();
    value.starts_with("http:") || value.starts_with("https:") || value.starts_with("//")
}

fn is_non_local_resource(resource: &str) -> bool {
    let value = resource.trim().to_ascii_lowercase();
    is_remote_resource(&value)
        || value.starts_with("data:")
        || value.starts_with("inkflow-asset://")
}

fn safe_relative_resource_paths(resource: &str) -> Option<Vec<PathBuf>> {
    let decoded = decode_resource_destination(resource)?;
    let decoded_path = safe_relative_resource_path_value(&decoded)?;
    let mut paths = vec![decoded_path];
    if decoded != resource {
        if let Some(literal_path) = safe_relative_resource_path_value(resource) {
            if !paths.contains(&literal_path) {
                paths.push(literal_path);
            }
        }
    }
    Some(paths)
}

fn safe_relative_resource_path_value(resource: &str) -> Option<PathBuf> {
    if is_non_local_resource(resource) {
        return None;
    }
    let path = PathBuf::from(resource.replace('/', std::path::MAIN_SEPARATOR_STR));
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::Prefix(_) | std::path::Component::RootDir
            )
        })
    {
        return None;
    }
    Some(path)
}

fn decode_resource_destination(resource: &str) -> Option<String> {
    let input = resource.as_bytes();
    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' && index + 2 < input.len() {
            if let (Some(high), Some(low)) =
                (hex_value(input[index + 1]), hex_value(input[index + 2]))
            {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        decoded.push(input[index]);
        index += 1;
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.contains('\0')).then_some(decoded)
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn asset_bytes_and_extension(request: &WriteAssetRequest) -> ApiResult<(Vec<u8>, String)> {
    if let Some(source) = request.source_path.as_deref() {
        let path = canonical_existing(Path::new(source))?;
        if !path.is_file() {
            return Err(ApiError::new(
                "invalid_asset",
                "The dropped asset is not a file.",
            ));
        }
        let metadata = fs::metadata(&path)
            .map_err(|error| ApiError::io("Unable to inspect the source image", error))?;
        if metadata.len() > MAX_IMAGE_BYTES {
            return Err(ApiError::new(
                "asset_too_large",
                "Images cannot exceed 50MB.",
            ));
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        if !is_image_path(&path) {
            return Err(ApiError::new(
                "invalid_asset",
                "The dropped file is not a supported image.",
            ));
        }
        let bytes = fs::read(path)
            .map_err(|error| ApiError::io("Unable to read the source image", error))?;
        return Ok((bytes, extension));
    }

    let encoded = request
        .data_base64
        .as_deref()
        .ok_or_else(|| ApiError::new("invalid_asset", "No image data was provided."))?;
    let payload = encoded
        .split_once(',')
        .map(|(_, data)| data)
        .unwrap_or(encoded);
    validate_base64_payload_length(payload.len())?;
    let bytes = STANDARD
        .decode(payload)
        .map_err(|error| ApiError::new("invalid_asset", error.to_string()))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ApiError::new(
            "asset_too_large",
            "Images cannot exceed 50MB.",
        ));
    }

    let extension = match request.mime_type.as_deref().unwrap_or("image/png") {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => {
            return Err(ApiError::new(
                "invalid_asset",
                "The pasted data is not a supported image.",
            ));
        }
    };
    Ok((bytes, extension.into()))
}

fn validate_base64_payload_length(length: usize) -> ApiResult<()> {
    if length > MAX_BASE64_IMAGE_BYTES {
        return Err(ApiError::new(
            "asset_too_large",
            "Images cannot exceed 50MB.",
        ));
    }
    Ok(())
}

fn find_existing_asset(
    directory: &Path,
    hash: &str,
    hash_prefix: &str,
    extension: &str,
) -> ApiResult<Option<PathBuf>> {
    let suffix = format!("-{hash_prefix}.{extension}");
    let content_addressed_name = format!("image-{hash}.{extension}");
    for item in fs::read_dir(directory)
        .map_err(|error| ApiError::io("Unable to scan the asset directory", error))?
    {
        let path = item
            .map_err(|error| ApiError::io("Unable to inspect an asset", error))?
            .path();
        if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name == content_addressed_name || name.ends_with(&suffix))
        {
            let bytes = fs::read(&path)
                .map_err(|error| ApiError::io("Unable to inspect an existing asset", error))?;
            if blake3::hash(&bytes).to_hex().as_str() == hash {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

fn safe_component(value: &str) -> ApiResult<&str> {
    let mut components = Path::new(value).components();
    if matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
    {
        Ok(value)
    } else {
        Err(ApiError::new(
            "invalid_asset_path",
            "Asset identifiers cannot contain directory components.",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageDestination {
    path: String,
    range: Range<usize>,
    syntax: ImageDestinationSyntax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageDestinationSyntax {
    Markdown { angle_wrapped: bool },
    Html { quote: Option<u8> },
}

fn markdown_image_patterns() -> [Regex; 2] {
    [
        Regex::new(r#"!\[[^\]\r\n]*\]\(<(?P<path>[^>\r\n]+)>(?:\s+[\"'][^)\r\n]*[\"'])?\)"#)
            .expect("valid angle image pattern"),
        Regex::new(r#"!\[[^\]\r\n]*\]\((?P<path>[^\s)\r\n]+)(?:\s+[\"'][^)\r\n]*[\"'])?\)"#)
            .expect("valid image pattern"),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HtmlImageAttributeKind {
    Src,
    Srcset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HtmlImageAttribute {
    kind: HtmlImageAttributeKind,
    range: Range<usize>,
    quote: Option<u8>,
}

/// Finds image-fetching attributes without treating `>` inside a quoted value
/// as the end of the tag. HTML also permits unquoted attribute values, so a
/// regular expression that only matches quoted `src`/`srcset` values is not
/// sufficient for Save As resource discovery.
fn html_image_attributes(source: &str) -> Vec<HtmlImageAttribute> {
    let bytes = source.as_bytes();
    let mut attributes = Vec::new();
    let mut cursor = 0usize;

    while cursor < bytes.len() {
        let Some(relative_start) = source[cursor..].find('<') else {
            break;
        };
        let tag_start = cursor + relative_start;

        if bytes[tag_start..].starts_with(b"<!--") {
            cursor = source[tag_start + 4..]
                .find("-->")
                .map(|offset| tag_start + 4 + offset + 3)
                .unwrap_or(bytes.len());
            continue;
        }

        let name_start = tag_start + 1;
        let Some((is_img, name_end)) = html_image_tag_name(bytes, name_start) else {
            cursor = name_start;
            continue;
        };
        let Some(tag_end) = html_tag_end(bytes, name_end) else {
            break;
        };

        collect_html_image_tag_attributes(bytes, name_end, tag_end, is_img, &mut attributes);
        cursor = tag_end + 1;
    }

    attributes
}

fn html_image_tag_name(bytes: &[u8], start: usize) -> Option<(bool, usize)> {
    for (name, is_img) in [(b"img".as_slice(), true), (b"source".as_slice(), false)] {
        let end = start.checked_add(name.len())?;
        if end <= bytes.len()
            && bytes[start..end].eq_ignore_ascii_case(name)
            && bytes
                .get(end)
                .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(*byte, b'/' | b'>'))
        {
            return Some((is_img, end));
        }
    }
    None
}

fn html_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, byte) in bytes[start..].iter().copied().enumerate() {
        match (quote, byte) {
            (Some(active), value) if value == active => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(start + offset),
            _ => {}
        }
    }
    None
}

fn collect_html_image_tag_attributes(
    bytes: &[u8],
    mut cursor: usize,
    tag_end: usize,
    is_img: bool,
    attributes: &mut Vec<HtmlImageAttribute>,
) {
    while cursor < tag_end {
        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag_end || bytes[cursor] == b'/' {
            break;
        }

        let name_start = cursor;
        while cursor < tag_end
            && !bytes[cursor].is_ascii_whitespace()
            && !matches!(bytes[cursor], b'/' | b'=' | b'>')
        {
            cursor += 1;
        }
        let name_end = cursor;
        if name_start == name_end {
            cursor += 1;
            continue;
        }

        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= tag_end || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < tag_end && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        let (value_start, value_end, quote) = match bytes.get(cursor).copied() {
            Some(quote @ (b'\'' | b'"')) => {
                cursor += 1;
                let start = cursor;
                while cursor < tag_end && bytes[cursor] != quote {
                    cursor += 1;
                }
                let end = cursor;
                if cursor < tag_end {
                    cursor += 1;
                }
                (start, end, Some(quote))
            }
            Some(_) => {
                let start = cursor;
                while cursor < tag_end && !bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                (start, cursor, None)
            }
            None => continue,
        };

        let name = &bytes[name_start..name_end];
        let kind = if is_img && name.eq_ignore_ascii_case(b"src") {
            Some(HtmlImageAttributeKind::Src)
        } else if name.eq_ignore_ascii_case(b"srcset") {
            Some(HtmlImageAttributeKind::Srcset)
        } else {
            None
        };
        if let Some(kind) = kind {
            attributes.push(HtmlImageAttribute {
                kind,
                range: value_start..value_end,
                quote,
            });
        }
    }
}

pub(crate) fn srcset_path_ranges(value: &str) -> Vec<Range<usize>> {
    let bytes = value.as_bytes();
    let mut ranges = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        while cursor < bytes.len() && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b',')
        {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }

        let start = cursor;
        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let mut end = cursor;
        while end > start && bytes[end - 1] == b',' {
            end -= 1;
        }
        let ended_with_comma = end < cursor;
        if end > start {
            ranges.push(start..end);
        }
        if ended_with_comma {
            continue;
        }

        let mut parentheses = 0usize;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'(' => parentheses += 1,
                b')' => parentheses = parentheses.saturating_sub(1),
                b',' if parentheses == 0 => {
                    cursor += 1;
                    break;
                }
                _ => {}
            }
            cursor += 1;
        }
    }
    ranges
}

fn normalize_reference_label(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn reference_definition_patterns() -> [Regex; 2] {
    [
        Regex::new(r#"(?m)^\s{0,3}\[(?P<label>[^\]\r\n]+)\]:\s*<(?P<path>[^>\r\n]+)>"#)
            .expect("valid angle reference definition"),
        Regex::new(r#"(?m)^\s{0,3}\[(?P<label>[^\]\r\n]+)\]:\s*(?P<path>[^<\s\r\n]+)"#)
            .expect("valid reference definition"),
    ]
}

fn collect_image_destinations(content: &str) -> Vec<ImageDestination> {
    let mut destinations = Vec::new();
    let mut reference_labels = HashSet::new();
    let parser = Parser::new_ext(content, Options::all());
    let reference_definitions: HashMap<String, (Range<usize>, String)> = parser
        .reference_definitions()
        .iter()
        .map(|(label, definition)| {
            (
                normalize_reference_label(label),
                (definition.span.clone(), definition.dest.to_string()),
            )
        })
        .collect();

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                id,
                ..
            }) => match link_type {
                LinkType::Inline => {
                    let source = &content[range.clone()];
                    for (index, pattern) in markdown_image_patterns().into_iter().enumerate() {
                        let Some(captures) = pattern.captures(source) else {
                            continue;
                        };
                        let path = captures.name("path").expect("image path");
                        destinations.push(ImageDestination {
                            // CommonMark resolves character references and
                            // backslash escapes before exposing a destination.
                            // Keep the source range for rewriting, but use the
                            // semantic URL when resolving a local file.
                            path: dest_url.to_string(),
                            range: range.start + path.start()..range.start + path.end(),
                            syntax: ImageDestinationSyntax::Markdown {
                                angle_wrapped: index == 0,
                            },
                        });
                        break;
                    }
                }
                LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut => {
                    reference_labels.insert(normalize_reference_label(id.as_ref()));
                }
                _ => {}
            },
            Event::Html(_) | Event::InlineHtml(_) => {
                let source = &content[range.clone()];
                for attribute in html_image_attributes(source) {
                    let quote = attribute.quote;
                    let candidate_ranges = match attribute.kind {
                        HtmlImageAttributeKind::Src => vec![attribute.range],
                        HtmlImageAttributeKind::Srcset => {
                            srcset_path_ranges(&source[attribute.range.clone()])
                                .into_iter()
                                .map(|candidate| {
                                    attribute.range.start + candidate.start
                                        ..attribute.range.start + candidate.end
                                })
                                .collect()
                        }
                    };
                    for candidate in candidate_ranges {
                        let start = range.start + candidate.start;
                        let end = range.start + candidate.end;
                        destinations.push(ImageDestination {
                            path: html_escape::decode_html_entities(&content[start..end])
                                .into_owned(),
                            range: start..end,
                            syntax: ImageDestinationSyntax::Html { quote },
                        });
                    }
                }
            }
            _ => {}
        }
    }

    for label in reference_labels {
        let Some((range, destination)) = reference_definitions.get(&label) else {
            continue;
        };
        let source = &content[range.clone()];
        for (index, pattern) in reference_definition_patterns().into_iter().enumerate() {
            let Some(captures) = pattern.captures(source) else {
                continue;
            };
            let path = captures.name("path").expect("definition path");
            destinations.push(ImageDestination {
                path: destination.clone(),
                range: range.start + path.start()..range.start + path.end(),
                syntax: ImageDestinationSyntax::Markdown {
                    angle_wrapped: index == 0,
                },
            });
            break;
        }
    }

    destinations.sort_by_key(|destination| destination.range.start);
    destinations.dedup_by(|left, right| left.range == right.range);
    destinations
}

fn rewrite_image_destinations(content: &str, replacements: &HashMap<String, String>) -> String {
    let mut rewritten = content.to_string();
    let mut destinations = collect_image_destinations(content);
    destinations.sort_by_key(|destination| std::cmp::Reverse(destination.range.start));
    for destination in destinations {
        if let Some(replacement) = replacements.get(&destination.path) {
            let replacement = replacement_for_destination(replacement, destination.syntax);
            rewritten.replace_range(destination.range, &replacement);
        }
    }
    rewritten
}

fn replacement_for_destination(replacement: &str, syntax: ImageDestinationSyntax) -> String {
    match syntax {
        ImageDestinationSyntax::Markdown {
            angle_wrapped: false,
        } if replacement
            .chars()
            .any(|character| character.is_whitespace() || matches!(character, '(' | ')')) =>
        {
            format!("<{replacement}>")
        }
        ImageDestinationSyntax::Html { quote } => {
            encode_html_attribute_replacement(replacement, quote)
        }
        _ => replacement.to_string(),
    }
}

fn encode_html_attribute_replacement(replacement: &str, quote: Option<u8>) -> String {
    let must_encode = |character: char| match quote {
        Some(quote) => character.is_ascii() && character as u8 == quote,
        None => {
            character.is_ascii_whitespace()
                || matches!(character, '"' | '\'' | '`' | '<' | '>' | '=')
        }
    };
    if !replacement.chars().any(must_encode) {
        return replacement.to_string();
    }

    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(replacement.len());
    for character in replacement.chars() {
        if must_encode(character) {
            let byte = character as u8;
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        } else {
            encoded.push(character);
        }
    }
    encoded
}

fn encode_generated_resource_path(path: &str) -> String {
    // Markdown destinations use URL semantics. A literal percent sequence in
    // a Windows file name must therefore be escaped before the renderer and
    // resource loader perform their single decoding pass. Ampersands are also
    // escaped so a valid filename cannot become an HTML/CommonMark character
    // reference when the rewritten document is parsed again.
    path.replace('%', "%25").replace('&', "%26")
}

fn asset_result(path: PathBuf, prefix: &str, pending: bool) -> WriteAssetResult {
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    let markdown_path = if pending {
        format!("inkflow-asset://{filename}")
    } else {
        encode_generated_resource_path(&format!("{prefix}/{filename}"))
    };
    WriteAssetResult {
        absolute_path: path.to_string_lossy().into_owned(),
        markdown_path,
    }
}

fn mime_for_extension(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_preview_and_write_use_the_same_content_addressed_path() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, "# Note\n").unwrap();
        let request = WriteAssetRequest {
            document_id: "document".into(),
            document_path: Some(document.to_string_lossy().into_owned()),
            source_path: None,
            data_base64: Some("aW1hZ2U=".into()),
            mime_type: Some("image/png".into()),
        };

        let preview = prepare_asset(temp.path(), request.clone(), false).unwrap();
        assert!(!Path::new(&preview.absolute_path).exists());

        let written = prepare_asset(temp.path(), request, true).unwrap();
        assert_eq!(preview.absolute_path, written.absolute_path);
        assert_eq!(preview.markdown_path, written.markdown_path);
        assert!(Path::new(&written.absolute_path).is_file());
        assert!(written.markdown_path.contains("image-"));
    }

    #[test]
    fn asset_preview_rejects_the_same_hash_path_collision_as_write() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, "# Note\n").unwrap();
        let request = WriteAssetRequest {
            document_id: "document".into(),
            document_path: Some(document.to_string_lossy().into_owned()),
            source_path: None,
            data_base64: Some("aW1hZ2U=".into()),
            mime_type: Some("image/png".into()),
        };
        let planned = prepare_asset(temp.path(), request.clone(), false).unwrap();
        let occupied = PathBuf::from(planned.absolute_path);
        fs::create_dir_all(occupied.parent().unwrap()).unwrap();
        fs::write(&occupied, b"different data").unwrap();

        let preview_error = prepare_asset(temp.path(), request.clone(), false).unwrap_err();
        let write_error = prepare_asset(temp.path(), request, true).unwrap_err();

        assert_eq!(preview_error.code, "asset_hash_collision");
        assert_eq!(write_error.code, "asset_hash_collision");
        assert_eq!(fs::read(occupied).unwrap(), b"different data");
    }

    #[test]
    fn save_as_copies_local_images_and_rewrites_relative_links() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("diagram.png"), b"png bytes").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "![diagram](draft.assets/diagram.png)",
            None,
        )
        .unwrap();

        assert_eq!(rewritten, "![diagram](published.assets/diagram.png)");
        assert_eq!(
            fs::read(temp.path().join("published.assets/diagram.png")).unwrap(),
            b"png bytes"
        );
    }

    #[test]
    fn save_as_preview_matches_rewritten_content_without_copying_assets() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("diagram.png"), b"png bytes").unwrap();

        let preview = preview_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "![diagram](draft.assets/diagram.png)",
            None,
        )
        .unwrap();

        assert_eq!(preview, "![diagram](published.assets/diagram.png)");
        assert!(!temp.path().join("published.assets").exists());
        assert_eq!(
            preview,
            copy_referenced_assets_for_save_as(
                &source_document,
                &destination_document,
                "![diagram](draft.assets/diagram.png)",
                None,
            )
            .unwrap()
        );
    }

    #[test]
    fn save_as_preview_reserves_distinct_names_for_same_named_assets() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let first_directory = temp.path().join("first");
        let second_directory = temp.path().join("second");
        fs::create_dir(&first_directory).unwrap();
        fs::create_dir(&second_directory).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(first_directory.join("image.png"), b"first image").unwrap();
        fs::write(second_directory.join("image.png"), b"second image").unwrap();
        let content = "![first](first/image.png)\n![second](second/image.png)";

        let preview = preview_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            content,
            None,
        )
        .unwrap();
        assert!(!temp.path().join("published.assets").exists());

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            content,
            None,
        )
        .unwrap();
        let destinations = rewritten.lines().collect::<Vec<_>>();

        assert_eq!(preview, rewritten);
        assert_eq!(destinations.len(), 2);
        assert_ne!(destinations[0], destinations[1]);
        assert_eq!(
            fs::read_dir(temp.path().join("published.assets"))
                .unwrap()
                .count(),
            2
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn save_as_destination_lock_serializes_transactions() {
        use std::{sync::mpsc, thread, time::Duration};

        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("published.md");
        let lock_path = save_as_lock_path(&destination).unwrap();
        let first = lock_save_as_destination(&destination).unwrap();
        // Both documents map to `published.assets` despite having different
        // filenames and must therefore share the same transaction lock.
        let worker_destination = temp.path().join("published.markdown");
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let second = lock_save_as_destination(&worker_destination).unwrap();
            acquired_tx.send(()).unwrap();
            second
        });

        started_rx.recv().unwrap();
        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "the competing transaction acquired the destination lock too early"
        );
        drop(first);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        drop(worker.join().unwrap());
        fs::remove_file(lock_path).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn saved_asset_write_waits_for_the_save_as_transaction() {
        use std::{sync::mpsc, thread, time::Duration};

        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("published.md");
        fs::write(&document, "# Published\n").unwrap();
        let first = lock_save_as_destination(&document).unwrap();
        let recovery = temp.path().join("Recovery");
        let worker_document = document.to_string_lossy().into_owned();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = write_asset(
                &recovery,
                WriteAssetRequest {
                    document_id: "published".into(),
                    document_path: Some(worker_document),
                    source_path: None,
                    data_base64: Some("cG5n".into()),
                    mime_type: Some("image/png".into()),
                },
            );
            finished_tx.send(result).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "the asset write escaped the Save As namespace lock"
        );
        drop(first);
        let result = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(Path::new(&result.absolute_path).is_file());
        worker.join().unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn saved_asset_write_waits_for_workspace_path_mutations() {
        use std::{sync::mpsc, thread, time::Duration};

        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("published.md");
        fs::write(&document, "# Published\n").unwrap();
        let first = lock_path_mutations().unwrap();
        let recovery = temp.path().join("Recovery");
        let worker_document = document.to_string_lossy().into_owned();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = write_asset(
                &recovery,
                WriteAssetRequest {
                    document_id: "published".into(),
                    document_path: Some(worker_document),
                    source_path: None,
                    data_base64: Some("cG5n".into()),
                    mime_type: Some("image/png".into()),
                },
            );
            finished_tx.send(result).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "the asset write escaped the workspace path mutation lock"
        );
        drop(first);
        let result = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(Path::new(&result.absolute_path).is_file());
        worker.join().unwrap();
    }

    #[test]
    fn tracked_save_as_copy_rolls_back_until_the_document_commits() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("diagram.png"), b"png bytes").unwrap();
        let content = "![diagram](draft.assets/diagram.png)";

        let rolled_back_destination = temp.path().join("rolled-back.md");
        let copy = copy_referenced_assets_for_save_as_tracked(
            &source_document,
            &rolled_back_destination,
            content,
            None,
        )
        .unwrap();
        assert_eq!(copy.content(), "![diagram](rolled-back.assets/diagram.png)");
        assert!(temp.path().join("rolled-back.assets/diagram.png").is_file());
        drop(copy);
        assert!(!temp.path().join("rolled-back.assets").exists());

        let committed_destination = temp.path().join("committed.md");
        let copy = copy_referenced_assets_for_save_as_tracked(
            &source_document,
            &committed_destination,
            content,
            None,
        )
        .unwrap();
        let rewritten = copy.commit();
        assert_eq!(rewritten, "![diagram](committed.assets/diagram.png)");
        assert!(temp.path().join("committed.assets/diagram.png").is_file());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn scoped_save_as_rejects_a_junction_before_resolving_an_image() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let actual = root.join("actual");
        let junction = root.join("linked");
        fs::create_dir_all(&actual).unwrap();
        let source_document = root.join("draft.md");
        let destination_document = root.join("published.md");
        fs::write(&source_document, "draft").unwrap();
        fs::write(actual.join("image.png"), b"image").unwrap();
        let created = std::process::Command::new("cmd.exe")
            .args(["/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&actual)
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );

        let result = preview_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "![image](linked/image.png)",
            Some(&root),
        );
        fs::remove_dir(&junction).unwrap();

        assert_eq!(result.unwrap_err().code, "reparse_point_blocked");
        assert!(!root.join("published.assets").exists());
    }

    #[test]
    fn save_as_only_rewrites_image_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("diagram.png"), b"png bytes").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "draft.assets/diagram.png\n![diagram](draft.assets/diagram.png)",
            None,
        )
        .unwrap();

        assert_eq!(
            rewritten,
            "draft.assets/diagram.png\n![diagram](published.assets/diagram.png)"
        );
    }

    #[test]
    fn save_as_handles_reference_and_html_images() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("one.png"), b"one").unwrap();
        fs::write(source_assets.join("two.png"), b"two").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "![one][asset]\n![shortcut]\n\n[asset]: draft.assets/one.png\n[shortcut]: draft.assets/two.png\n<img src=\"draft.assets/two.png\">",
            None,
        )
        .unwrap();

        assert!(rewritten.contains("[asset]: published.assets/one.png"));
        assert!(rewritten.contains("[shortcut]: published.assets/two.png"));
        assert!(rewritten.contains("src=\"published.assets/two.png\""));
    }

    #[test]
    fn save_as_copies_and_rewrites_picture_srcset_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("wide.png"), b"wide").unwrap();
        fs::write(source_assets.join("retina.png"), b"retina").unwrap();
        fs::write(source_assets.join("fallback.png"), b"fallback").unwrap();
        let content = concat!(
            "<picture>\n",
            "  <source srcset=\"draft.assets/wide.png 1x, draft.assets/retina.png 2x\">\n",
            "  <img src='draft.assets/fallback.png' srcset='draft.assets/wide.png 640w'>\n",
            "</picture>\n",
        );

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            content,
            None,
        )
        .unwrap();

        assert!(
            rewritten.contains(
                "srcset=\"published.assets/wide.png 1x, published.assets/retina.png 2x\""
            )
        );
        assert!(rewritten.contains("src='published.assets/fallback.png'"));
        assert!(rewritten.contains("srcset='published.assets/wide.png 640w'"));
        for name in ["wide.png", "retina.png", "fallback.png"] {
            assert!(temp.path().join("published.assets").join(name).is_file());
        }
    }

    #[test]
    fn save_as_parses_quoted_gt_and_unquoted_html_image_attributes() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        fs::write(&source_document, "draft").unwrap();
        fs::write(temp.path().join("wide.png"), b"wide").unwrap();
        fs::write(temp.path().join("fallback.png"), b"fallback").unwrap();
        let content = concat!(
            "<picture>\n",
            "  <source media=\"(width > 600px)\" srcset=\"wide.png 2x\">\n",
            "  <img alt='width > height' src=fallback.png srcset=fallback.png>\n",
            "</picture>\n",
        );

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            content,
            None,
        )
        .unwrap();

        assert!(rewritten.contains("media=\"(width > 600px)\""));
        assert!(rewritten.contains("srcset=\"published.assets/wide.png 2x\""));
        assert!(
            rewritten
                .contains("src=published.assets/fallback.png srcset=published.assets/fallback.png")
        );
        assert!(temp.path().join("published.assets/wide.png").is_file());
        assert!(temp.path().join("published.assets/fallback.png").is_file());
    }

    #[test]
    fn save_as_keeps_parsing_after_a_descriptorless_srcset_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        fs::write(&source_document, "draft").unwrap();
        fs::write(temp.path().join("one.png"), b"one").unwrap();
        fs::write(temp.path().join("two.png"), b"two").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "<img srcset=\"one.png, two.png 2x\">",
            None,
        )
        .unwrap();

        assert_eq!(
            rewritten,
            "<img srcset=\"published.assets/one.png, published.assets/two.png 2x\">"
        );
        assert!(temp.path().join("published.assets/one.png").is_file());
        assert!(temp.path().join("published.assets/two.png").is_file());
    }

    #[test]
    fn save_as_decodes_character_references_before_resolving_images() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_asset = temp.path().join("a&copy;.png");
        fs::write(&source_document, "draft").unwrap();
        fs::write(&source_asset, b"image").unwrap();
        let content = concat!(
            "![inline](a&amp;copy;.png)\n",
            "![reference][asset]\n\n",
            "[asset]: a&#38;copy;.png\n",
            "<img src=\"a&#x26;copy;.png\" srcset=\"a&amp;copy;.png 2x\">",
        );

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            content,
            None,
        )
        .unwrap();

        assert_eq!(
            rewritten,
            concat!(
                "![inline](published.assets/a%26copy;.png)\n",
                "![reference][asset]\n\n",
                "[asset]: published.assets/a%26copy;.png\n",
                "<img src=\"published.assets/a%26copy;.png\" srcset=\"published.assets/a%26copy;.png 2x\">",
            )
        );
        assert_eq!(
            fs::read(temp.path().join("published.assets/a&copy;.png")).unwrap(),
            b"image"
        );
    }

    #[test]
    fn save_as_keeps_entity_decoded_html_attributes_syntactically_valid() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        fs::write(&source_document, "draft").unwrap();
        fs::write(temp.path().join("my image.png"), b"space").unwrap();
        fs::write(temp.path().join("author's.png"), b"quote").unwrap();
        let content = concat!(
            "<img src=my&#32;image.png>\n",
            "<img src='author&apos;s.png'>",
        );

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            content,
            None,
        )
        .unwrap();

        assert_eq!(
            rewritten,
            concat!(
                "<img src=published.assets/my%20image.png>\n",
                "<img src='published.assets/author%27s.png'>",
            )
        );
        assert_eq!(
            fs::read(temp.path().join("published.assets/my image.png")).unwrap(),
            b"space"
        );
        assert_eq!(
            fs::read(temp.path().join("published.assets/author's.png")).unwrap(),
            b"quote"
        );
    }

    #[test]
    fn pending_asset_ids_cannot_escape_the_recovery_directory() {
        let temp = tempfile::tempdir().unwrap();
        let result = write_asset(
            temp.path(),
            WriteAssetRequest {
                document_id: "../outside".into(),
                document_path: None,
                source_path: None,
                data_base64: Some("aW1hZ2U=".into()),
                mime_type: Some("image/png".into()),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn save_as_does_not_rewrite_image_examples_in_code() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("diagram.png"), b"png bytes").unwrap();
        let content = concat!(
            "`![inline](draft.assets/diagram.png)`\n\n",
            "```markdown\n![fenced][example]\n[example]: draft.assets/diagram.png\n```\n\n",
            "![actual][asset]\n\n[asset]: draft.assets/diagram.png\n",
        );

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            content,
            None,
        )
        .unwrap();

        assert!(rewritten.contains("`![inline](draft.assets/diagram.png)`"));
        assert!(rewritten.contains("[example]: draft.assets/diagram.png"));
        assert!(rewritten.contains("[asset]: published.assets/diagram.png"));
    }

    #[test]
    fn rejects_oversized_base64_before_decoding() {
        assert!(validate_base64_payload_length(MAX_BASE64_IMAGE_BYTES).is_ok());
        assert!(validate_base64_payload_length(MAX_BASE64_IMAGE_BYTES + 1).is_err());
    }

    #[test]
    fn migration_keeps_pending_assets_until_save_commits() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        let pending = recovery.join("assets").join("document");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("image.png"), b"image").unwrap();
        let document = temp.path().join("note.md");
        let lock = lock_pending_assets(&recovery).unwrap();

        let rewritten = migrate_pending_assets(
            &lock,
            "document",
            &document,
            "![image](inkflow-asset://image.png)",
        )
        .unwrap();

        assert_eq!(rewritten, "![image](note.assets/image.png)");
        assert!(pending.join("image.png").exists());
        cleanup_pending_assets(&lock, "document", "![image](inkflow-asset://image.png)").unwrap();
        assert!(!pending.exists());
    }

    #[test]
    fn migration_and_cleanup_leave_unreferenced_pending_uploads_for_a_later_save() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        let pending = recovery.join("assets").join("document");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("ready.png"), b"ready").unwrap();
        fs::write(pending.join("uploading.png"), b"uploading").unwrap();
        let document = temp.path().join("note.md");
        let content = "![ready](inkflow-asset://ready.png)\n![uploading](inkflow-upload://token)";
        let lock = lock_pending_assets(&recovery).unwrap();

        let rewritten = migrate_pending_assets(&lock, "document", &document, content).unwrap();
        cleanup_pending_assets(&lock, "document", content).unwrap();

        assert_eq!(
            rewritten,
            "![ready](note.assets/ready.png)\n![uploading](inkflow-upload://token)"
        );
        assert!(!pending.join("ready.png").exists());
        assert!(pending.join("uploading.png").exists());
    }

    #[test]
    fn migration_wraps_markdown_asset_paths_that_contain_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        let pending = recovery.join("assets").join("document");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("image.png"), b"image").unwrap();
        let document = temp.path().join("My Note.md");
        let placeholder = "inkflow-asset://image.png";
        let content = format!(
            "![inline]({placeholder})\n![reference][asset]\n\n[asset]: {placeholder}\n<img src=\"{placeholder}\">"
        );
        let lock = lock_pending_assets(&recovery).unwrap();

        let rewritten = migrate_pending_assets(&lock, "document", &document, &content).unwrap();

        assert_eq!(
            rewritten,
            "![inline](<My Note.assets/image.png>)\n![reference][asset]\n\n[asset]: <My Note.assets/image.png>\n<img src=\"My Note.assets/image.png\">"
        );
    }

    #[test]
    fn migration_rejects_directory_components_as_document_ids() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        fs::create_dir_all(recovery.join("assets")).unwrap();
        let document = temp.path().join("note.md");
        let lock = lock_pending_assets(&recovery).unwrap();

        assert!(migrate_pending_assets(&lock, ".", &document, "content").is_err());
        assert!(migrate_pending_assets(&lock, "..", &document, "content").is_err());
        assert!(cleanup_pending_assets(&lock, ".", "content").is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn pending_asset_writes_cannot_be_deleted_by_concurrent_cleanup() {
        use std::{sync::mpsc, thread, time::Duration};

        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        let pending = recovery.join("assets").join("document");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("old.png"), b"old").unwrap();
        let lock = lock_pending_assets(&recovery).unwrap();
        let worker_recovery = recovery.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            write_asset(
                &worker_recovery,
                WriteAssetRequest {
                    document_id: "document".into(),
                    document_path: None,
                    source_path: None,
                    data_base64: Some(STANDARD.encode(b"new")),
                    mime_type: Some("image/png".into()),
                },
            )
        });
        started_rx.recv().unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(!worker.is_finished());

        cleanup_pending_assets(&lock, "document", "![old](inkflow-asset://old.png)").unwrap();
        drop(lock);

        let result = worker.join().unwrap().unwrap();
        assert!(Path::new(&result.absolute_path).exists());
        assert!(pending.exists());
    }

    #[test]
    fn save_as_copies_images_from_the_open_workspace_scope() {
        let temp = tempfile::tempdir().unwrap();
        let docs = temp.path().join("docs");
        let images = temp.path().join("images");
        fs::create_dir(&docs).unwrap();
        fs::create_dir(&images).unwrap();
        let source_document = docs.join("draft.md");
        let destination_document = temp.path().join("published.md");
        fs::write(&source_document, "draft").unwrap();
        fs::write(images.join("diagram.png"), b"png bytes").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "![diagram](../images/diagram.png)",
            Some(temp.path()),
        )
        .unwrap();

        assert_eq!(rewritten, "![diagram](published.assets/diagram.png)");
        assert!(temp.path().join("published.assets/diagram.png").is_file());
    }

    #[test]
    fn save_as_still_works_after_the_source_document_was_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("deleted.md");
        let destination_document = temp.path().join("rescued.md");
        fs::write(&source_document, "local edits").unwrap();
        fs::remove_file(&source_document).unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "local edits",
            Some(temp.path()),
        )
        .unwrap();

        assert_eq!(rewritten, "local edits");
    }

    #[test]
    fn unrelated_workspace_does_not_block_document_local_images() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let external = temp.path().join("external");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&external).unwrap();
        let document = external.join("note.md");
        fs::write(&document, "note").unwrap();
        fs::write(external.join("diagram.png"), b"png bytes").unwrap();

        let loaded = read_resource(&document, Some(&workspace), "diagram.png").unwrap();

        assert!(loaded.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn resource_loading_decodes_rendered_url_paths_with_spaces() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("My Note.md");
        let assets = temp.path().join("My Note.assets");
        fs::create_dir(&assets).unwrap();
        fs::write(&document, "![diagram](<My Note.assets/image.png>)").unwrap();
        fs::write(assets.join("image.png"), b"png bytes").unwrap();

        let loaded = read_resource(&document, None, "My%20Note.assets/image.png").unwrap();

        assert!(loaded.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn resource_loading_falls_back_to_legacy_literal_percent_paths() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("100%20done.md");
        let assets = temp.path().join("100%20done.assets");
        fs::create_dir(&assets).unwrap();
        fs::write(&document, "![diagram](100%20done.assets/image.png)").unwrap();
        fs::write(assets.join("image.png"), b"png bytes").unwrap();

        let loaded = read_resource(&document, None, "100%20done.assets/image.png").unwrap();

        assert!(loaded.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn save_as_copies_images_from_legacy_literal_percent_paths() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("100%20done.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("100%20done.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("image.png"), b"png bytes").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "![diagram](100%20done.assets/image.png)",
            None,
        )
        .unwrap();

        assert_eq!(rewritten, "![diagram](published.assets/image.png)");
        assert_eq!(
            fs::read(temp.path().join("published.assets/image.png")).unwrap(),
            b"png bytes"
        );
    }

    #[test]
    fn generated_asset_paths_escape_literal_percent_and_ampersand_sequences() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("100%20&copy;.md");
        fs::write(&document, "note").unwrap();

        let result = write_asset(
            temp.path(),
            WriteAssetRequest {
                document_id: "document".into(),
                document_path: Some(document.to_string_lossy().into_owned()),
                source_path: None,
                data_base64: Some("aW1hZ2U=".into()),
                mime_type: Some("image/png".into()),
            },
        )
        .unwrap();

        assert!(result.markdown_path.starts_with("100%2520%26copy;.assets/"));
        let loaded = read_resource(&document, None, &result.markdown_path).unwrap();
        assert!(loaded.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn decoded_resource_paths_are_rechecked_against_the_document_scope() {
        let temp = tempfile::tempdir().unwrap();
        let documents = temp.path().join("documents");
        fs::create_dir(&documents).unwrap();
        let document = documents.join("note.md");
        fs::write(&document, "note").unwrap();
        fs::write(temp.path().join("outside.png"), b"outside").unwrap();

        let error = read_resource(&document, None, "%2E%2E%2Foutside.png").unwrap_err();

        assert_eq!(error.code, "resource_outside_scope");
    }

    #[test]
    fn resource_loading_rejects_absolute_and_network_paths_before_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, "note").unwrap();

        let absolute = temp.path().join("image.png").to_string_lossy().into_owned();
        let absolute_error = read_resource(&document, None, &absolute).unwrap_err();
        let network_error = read_resource(&document, None, "//server/share/image.png").unwrap_err();
        let normalized_network_error =
            read_resource(&document, None, r"https:\\example.com\image.png").unwrap_err();
        let single_slash_network_error =
            read_resource(&document, None, "https:/example.com/image.png").unwrap_err();
        let scheme_relative_network_error =
            read_resource(&document, None, "https:example.com/image.png").unwrap_err();

        assert_eq!(absolute_error.code, "resource_outside_scope");
        assert_eq!(network_error.code, "remote_resource_blocked");
        assert_eq!(normalized_network_error.code, "remote_resource_blocked");
        assert_eq!(single_slash_network_error.code, "remote_resource_blocked");
        assert_eq!(
            scheme_relative_network_error.code,
            "remote_resource_blocked"
        );
    }

    #[test]
    fn save_as_leaves_protocol_relative_images_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        fs::write(&source_document, "draft").unwrap();
        let content = "![remote](//example.com/image.png)";

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            content,
            None,
        )
        .unwrap();

        assert_eq!(rewritten, content);
    }
}
