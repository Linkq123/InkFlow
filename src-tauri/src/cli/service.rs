use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::LazyLock,
};

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use regex::Regex;

use crate::{
    asset,
    data_lock::lock_path_mutations,
    encoding,
    error::{ApiError, ApiResult},
    fileio::{
        AtomicWriteOutcome, DirectoryIdentityGuard, atomic_create_if_absent,
        atomic_replace_existing, atomic_write_if_revision, canonical_existing, ensure_within,
        is_symbolic_link_or_junction, revision, revision_from_bytes,
    },
    model::{CheckpointRequest, DiskRevision, WriteAssetRequest, WriteAssetResult},
    recovery::RecoveryStore,
};

pub(super) use crate::destination::DestinationSnapshot;

use super::model::{
    DocumentAnalysis, DocumentInfo, DocumentMutationOutcome, DocumentStats, OutlineItem,
    TextPosition, TextRange,
};

pub const CLI_DATA_ENV: &str = "INKFLOW_DATA_DIR";

pub struct CliContext {
    pub data_dir: PathBuf,
    pub root: Option<PathBuf>,
}

impl CliContext {
    pub fn new(data_dir: Option<PathBuf>, root: Option<PathBuf>) -> ApiResult<Self> {
        let data_dir = match data_dir.or_else(|| std::env::var_os(CLI_DATA_ENV).map(PathBuf::from))
        {
            Some(path) if path.is_absolute() => path,
            Some(_) => {
                return Err(ApiError::new(
                    "invalid_data_directory",
                    "--data-dir and INKFLOW_DATA_DIR must contain an absolute path.",
                ));
            }
            None => crate::default_application_data_directory().map_err(|error| {
                ApiError::io("Unable to resolve the InkFlow data directory", error)
            })?,
        };
        fs::create_dir_all(&data_dir)
            .map_err(|error| ApiError::io("Unable to create the InkFlow data directory", error))?;
        let data_dir = canonical_existing(&data_dir)?;
        let root = root
            .map(|path| {
                let candidate = resolve_from_current_directory(&path)?;
                reject_reparse_path(&candidate)?;
                canonical_existing(&candidate)
            })
            .transpose()?;
        if let Some(root) = root.as_ref() {
            if !root.is_dir() {
                return Err(ApiError::new(
                    "not_a_directory",
                    "--root must be a directory.",
                ));
            }
            reject_reparse_points(root, root)?;
        }
        Ok(Self { data_dir, root })
    }

    pub fn recovery(&self) -> ApiResult<RecoveryStore> {
        RecoveryStore::new(self.data_dir.join("Recovery"))
    }

    pub fn existing_path(&self, input: &Path) -> ApiResult<PathBuf> {
        let candidate = resolve_from_current_directory(input)?;
        if self.root.is_some() {
            reject_reparse_path(&candidate)?;
        }
        let resolved = canonical_existing(&candidate)?;
        if let Some(root) = self.root.as_ref() {
            let scoped = ensure_within(root, &resolved)?;
            reject_reparse_points(root, &scoped)?;
            Ok(scoped)
        } else {
            Ok(resolved)
        }
    }

    pub fn existing_file(&self, input: &Path) -> ApiResult<PathBuf> {
        let path = self.existing_path(input)?;
        if !path.is_file() {
            return Err(ApiError::new(
                "not_a_file",
                "The document path is not a file.",
            ));
        }
        Ok(path)
    }

    pub fn destination_path(&self, input: &Path) -> ApiResult<PathBuf> {
        let candidate = resolve_from_current_directory(input)?;
        if self.root.is_some() {
            reject_reparse_path(if candidate.exists() {
                &candidate
            } else {
                candidate.parent().unwrap_or(&candidate)
            })?;
        }
        let resolved = if let Some(root) = self.root.as_ref() {
            let scoped = ensure_within(root, &candidate)?;
            reject_reparse_points(root, scoped.parent().unwrap_or(root))?;
            scoped
        } else if candidate.exists() {
            canonical_existing(&candidate)?
        } else {
            let parent = candidate.parent().ok_or_else(|| {
                ApiError::new("invalid_path", "The destination has no parent directory.")
            })?;
            canonical_existing(parent)?.join(candidate.file_name().ok_or_else(|| {
                ApiError::new("invalid_path", "The destination has no file name.")
            })?)
        };
        Ok(resolved)
    }

    pub(super) fn capture_destination(&self, input: &Path) -> ApiResult<DestinationSnapshot> {
        let path = self.destination_path(input)?;
        DestinationSnapshot::capture_resolved(path)
    }

    pub(super) fn capture_file_destination(
        &self,
        input: &Path,
    ) -> ApiResult<(DestinationSnapshot, Option<DiskRevision>)> {
        let path = self.destination_path(input)?;
        DestinationSnapshot::capture_file_resolved(path)
    }

    pub(super) fn revalidate_destination(
        &self,
        snapshot: &DestinationSnapshot,
    ) -> ApiResult<DirectoryIdentityGuard> {
        let path = self.destination_path(snapshot.path())?;
        snapshot.revalidate_resolved(&path)
    }

    /// Validates a logical path against `--root` even when one or more trailing
    /// directories no longer exist. Recovery records need this weaker existence
    /// requirement because deleting a document tree is precisely when the
    /// snapshot may be needed.
    pub fn scoped_path_allow_missing(&self, input: &Path) -> ApiResult<PathBuf> {
        let Some(root) = self.root.as_ref() else {
            return self.destination_path(input);
        };
        let candidate = normalize_absolute_path(&resolve_from_current_directory(input)?)?;
        reject_reparse_path(&candidate)?;
        let existing_ancestor = candidate
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .ok_or_else(|| ApiError::new("invalid_path", "The path has no existing ancestor."))?;
        let canonical_ancestor = canonical_existing(existing_ancestor)?;
        if !canonical_ancestor.starts_with(root) {
            return Err(ApiError::new(
                "path_outside_workspace",
                "The path is outside the open workspace.",
            ));
        }
        reject_reparse_points(root, &canonical_ancestor)?;
        let suffix = candidate.strip_prefix(existing_ancestor).map_err(|_| {
            ApiError::new(
                "invalid_path",
                "Unable to resolve the missing path components.",
            )
        })?;
        Ok(canonical_ancestor.join(suffix))
    }
}

fn normalize_absolute_path(path: &Path) -> ApiResult<PathBuf> {
    if !path.is_absolute() {
        return Err(ApiError::new("invalid_path", "The path must be absolute."));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(ApiError::new(
                        "invalid_path",
                        "The path traverses above the filesystem root.",
                    ));
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

pub fn read_document(context: &CliContext, path: &Path) -> ApiResult<DocumentInfo> {
    let path = context.existing_file(path)?;
    let bytes =
        fs::read(&path).map_err(|error| ApiError::io("Unable to read the document", error))?;
    let decoded = encoding::decode(&bytes)?;
    let metadata = fs::metadata(&path)
        .map_err(|error| ApiError::io("Unable to inspect the document", error))?;
    let disk_revision = revision_from_bytes(&path, &bytes)?;
    Ok(DocumentInfo {
        path: Some(path.to_string_lossy().into_owned()),
        title: path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Untitled.md")
            .to_string(),
        content: decoded.content,
        encoding: decoded.encoding,
        eol: decoded.eol,
        had_bom: decoded.had_bom,
        had_final_newline: decoded.had_final_newline,
        read_only: metadata.permissions().readonly(),
        revision: Some(disk_revision.into()),
    })
}

pub fn stdin_document(content: String) -> DocumentInfo {
    DocumentInfo {
        path: None,
        title: "stdin.md".into(),
        had_final_newline: content.ends_with('\n') || content.ends_with('\r'),
        content: content.replace("\r\n", "\n").replace('\r', "\n"),
        encoding: "utf-8".into(),
        eol: "lf".into(),
        had_bom: false,
        read_only: false,
        revision: None,
    }
}

pub fn analyze_document(content: &str) -> DocumentAnalysis {
    DocumentAnalysis {
        stats: document_stats(content),
        outline: extract_outline(content),
        has_remote_images: has_remote_images(content),
    }
}

pub fn save_mutation(
    context: &CliContext,
    document: &DocumentInfo,
    content: String,
    operations: Vec<super::model::AppliedOperation>,
    dry_run: bool,
) -> ApiResult<DocumentMutationOutcome> {
    save_mutation_with_hook(context, document, content, operations, dry_run, || Ok(()))
}

fn save_mutation_with_hook<F>(
    context: &CliContext,
    document: &DocumentInfo,
    content: String,
    operations: Vec<super::model::AppliedOperation>,
    dry_run: bool,
    before_commit: F,
) -> ApiResult<DocumentMutationOutcome>
where
    F: FnOnce() -> ApiResult<()>,
{
    // Workspace rename/trash must not invalidate the resolved path after the
    // guarded write but before its final revision is reported.
    let _path_guard = lock_path_mutations()?;
    let path =
        PathBuf::from(document.path.as_deref().ok_or_else(|| {
            ApiError::new("missing_path", "A file path is required for mutation.")
        })?);
    let previous_revision = document.revision.clone();
    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    if content == document.content {
        if !dry_run {
            let expected = previous_revision
                .as_ref()
                .ok_or_else(|| {
                    ApiError::new("missing_revision", "The document has no disk revision.")
                })?
                .to_disk_revision()?;
            let current = if path.is_file() {
                match context
                    .existing_file(&path)
                    .and_then(|path| revision(&path))
                {
                    Ok(revision) => Some(revision),
                    Err(_) if !path.exists() => None,
                    Err(error) => return Err(error),
                }
            } else if path.exists() {
                return Err(conflict_error(None));
            } else {
                None
            };
            if current.as_ref() != Some(&expected) {
                return Err(conflict_error(current));
            }
        }
        return Ok(DocumentMutationOutcome {
            path: path.to_string_lossy().into_owned(),
            changed: false,
            dry_run,
            previous_revision: previous_revision.clone(),
            revision: previous_revision,
            content_hash,
            operations,
            changed_ranges: Vec::new(),
            diff: None,
        });
    }
    let changed_ranges = changed_ranges(&document.content, &content);
    if dry_run {
        return Ok(DocumentMutationOutcome {
            path: path.to_string_lossy().into_owned(),
            changed: true,
            dry_run: true,
            previous_revision,
            revision: None,
            content_hash,
            operations,
            changed_ranges,
            diff: Some(diff_preview(&path, &document.content, &content)),
        });
    }

    let destination = context.capture_destination(&path)?;
    before_commit()?;
    let _destination_guard = context.revalidate_destination(&destination)?;
    checkpoint_previous(context, document)?;
    let bytes = encoding::encode(
        &content,
        &document.encoding,
        &document.eol,
        document.had_bom,
    )?;
    let expected: DiskRevision = document
        .revision
        .as_ref()
        .ok_or_else(|| ApiError::new("missing_revision", "The document has no disk revision."))?
        .to_disk_revision()?;
    match atomic_write_if_revision(&path, &bytes, Some(&expected))? {
        AtomicWriteOutcome::Written => {}
        AtomicWriteOutcome::Conflict(current) => {
            return Err(conflict_error(current));
        }
    }
    let updated = revision_from_bytes(&path, &bytes)?;
    Ok(DocumentMutationOutcome {
        path: path.to_string_lossy().into_owned(),
        changed: true,
        dry_run: false,
        previous_revision,
        revision: Some(updated.into()),
        content_hash,
        operations,
        changed_ranges,
        diff: None,
    })
}

pub struct WriteOptions<'a> {
    pub content: &'a str,
    pub expected_hash: Option<&'a str>,
    pub force: bool,
    pub create: bool,
    pub encoding: Option<&'a str>,
    pub eol: Option<&'a str>,
    pub bom: Option<bool>,
    pub dry_run: bool,
}

pub fn write_document(
    context: &CliContext,
    path: &Path,
    options: WriteOptions<'_>,
) -> ApiResult<DocumentMutationOutcome> {
    let _path_guard = lock_path_mutations()?;
    write_document_with_hook(context, path, options, || Ok(()))
}

fn write_document_with_hook<F>(
    context: &CliContext,
    path: &Path,
    options: WriteOptions<'_>,
    before_unchanged_commit: F,
) -> ApiResult<DocumentMutationOutcome>
where
    F: FnOnce() -> ApiResult<()>,
{
    let destination = context.capture_destination(path)?;
    let path = destination.path().to_path_buf();
    let existing = path.exists();
    if existing && options.create {
        return Err(ApiError::new(
            "already_exists",
            "--create refuses to overwrite an existing file.",
        ));
    }
    if !existing && options.expected_hash.is_some() {
        return Err(ApiError::new(
            "revision_conflict",
            "The expected document no longer exists.",
        ));
    }
    if !existing && !options.create {
        return Err(ApiError::new(
            "confirmation_required",
            "Creating a file requires --create.",
        ));
    }

    let mut baseline = if existing {
        Some(read_document(context, &path)?)
    } else {
        None
    };
    if let Some(document) = baseline.as_ref() {
        let actual_hash = document.revision.as_ref().map(|value| value.hash.as_str());
        if !options.force && options.expected_hash.is_none() {
            return Err(ApiError::new(
                "confirmation_required",
                "Overwriting an existing document requires --expected-hash or --force.",
            ));
        }
        if let Some(expected) = options.expected_hash {
            if actual_hash != Some(expected) {
                return Err(ApiError::new(
                    "revision_conflict",
                    "The document hash does not match --expected-hash.",
                ));
            }
        }
    }

    let requested_encoding = options.encoding.map(encoding::canonical_name).transpose()?;
    let encoding_name = requested_encoding
        .as_deref()
        .or_else(|| baseline.as_ref().map(|value| value.encoding.as_str()))
        .unwrap_or("utf-8");
    let eol = options
        .eol
        .or_else(|| baseline.as_ref().map(|value| value.eol.as_str()))
        .unwrap_or("lf");
    if !matches!(eol.to_ascii_lowercase().as_str(), "lf" | "crlf") {
        return Err(ApiError::new("invalid_eol", "--eol must be lf or crlf."));
    }
    if options.bom == Some(true) && !encoding::supports_bom(encoding_name) {
        return Err(ApiError::new(
            "invalid_bom",
            "--bom is only supported with UTF-8, UTF-16LE, or UTF-16BE.",
        ));
    }
    if options.bom == Some(false) && matches!(encoding_name, "utf-16le" | "utf-16be") {
        return Err(ApiError::new(
            "invalid_bom",
            "UTF-16LE and UTF-16BE require a BOM so InkFlow can identify their byte order.",
        ));
    }
    let had_bom = match options.bom {
        Some(value) => value,
        None => match (requested_encoding.as_deref(), baseline.as_ref()) {
            (None, Some(document)) => document.had_bom,
            (Some(requested), Some(document))
                if encoding::canonical_name(&document.encoding)? == requested =>
            {
                document.had_bom
            }
            (Some("utf-16le" | "utf-16be"), _) => true,
            _ => false,
        },
    };
    let content = encoding::normalize_eol(options.content);
    let bytes = encoding::encode(&content, encoding_name, eol, had_bom)?;
    let encoded_hash = blake3::hash(&bytes).to_hex().to_string();
    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();
    let initially_unchanged = baseline
        .as_ref()
        .and_then(|document| document.revision.as_ref())
        .is_some_and(|revision| revision.hash == encoded_hash);
    if initially_unchanged && !options.dry_run {
        before_unchanged_commit()?;
        let refreshed = if path.is_file() {
            match read_document(context, &path) {
                Ok(document) => Some(document),
                Err(_) if !path.exists() => None,
                Err(error) => return Err(error),
            }
        } else if path.exists() {
            return Err(conflict_error(None));
        } else {
            None
        };
        let baseline_revision = baseline
            .as_ref()
            .and_then(|document| document.revision.as_ref());
        let refreshed_revision = refreshed
            .as_ref()
            .and_then(|document| document.revision.as_ref());
        if refreshed_revision != baseline_revision {
            if !options.force {
                let current = refreshed_revision
                    .map(|revision| revision.to_disk_revision())
                    .transpose()?;
                return Err(conflict_error(current));
            }
            baseline = refreshed;
        }
    }
    let previous_content = baseline
        .as_ref()
        .map(|value| value.content.as_str())
        .unwrap_or("");
    let changed_ranges = changed_ranges(previous_content, &content);
    let previous_revision = baseline.as_ref().and_then(|value| value.revision.clone());
    let changed = previous_revision
        .as_ref()
        .is_none_or(|revision| revision.hash != encoded_hash);
    if !changed {
        return Ok(DocumentMutationOutcome {
            path: path.to_string_lossy().into_owned(),
            changed: false,
            dry_run: options.dry_run,
            previous_revision: previous_revision.clone(),
            revision: (!options.dry_run).then_some(previous_revision).flatten(),
            content_hash,
            operations: Vec::new(),
            changed_ranges: Vec::new(),
            diff: None,
        });
    }
    if options.dry_run {
        return Ok(DocumentMutationOutcome {
            path: path.to_string_lossy().into_owned(),
            changed: true,
            dry_run: true,
            previous_revision,
            revision: None,
            content_hash,
            operations: Vec::new(),
            changed_ranges,
            diff: (previous_content != content)
                .then(|| diff_preview(&path, previous_content, &content)),
        });
    }

    let _destination_guard = context.revalidate_destination(&destination)?;
    if let Some(document) = baseline.as_ref() {
        checkpoint_previous(context, document)?;
    }
    let write_outcome = if let Some(document) = baseline.as_ref() {
        if options.force {
            atomic_replace_existing(&path, &bytes)?
        } else {
            let expected: DiskRevision = document
                .revision
                .as_ref()
                .expect("disk document revision")
                .to_disk_revision()?;
            atomic_write_if_revision(&path, &bytes, Some(&expected))?
        }
    } else if options.force && existing {
        atomic_replace_existing(&path, &bytes)?
    } else {
        atomic_create_if_absent(&path, &bytes)?
    };
    if let AtomicWriteOutcome::Conflict(current) = write_outcome {
        return Err(conflict_error(current));
    }
    let updated = revision_from_bytes(&path, &bytes)?;
    Ok(DocumentMutationOutcome {
        path: path.to_string_lossy().into_owned(),
        changed: true,
        dry_run: false,
        previous_revision,
        revision: Some(updated.into()),
        content_hash,
        operations: Vec::new(),
        changed_ranges,
        diff: None,
    })
}

pub fn save_document_as(
    context: &CliContext,
    source: &Path,
    destination: &Path,
    expected_destination_hash: Option<&str>,
    force: bool,
    dry_run: bool,
) -> ApiResult<DocumentMutationOutcome> {
    // Lock ordering is path mutation -> Save As namespace -> recovery.
    let _path_guard = lock_path_mutations()?;
    let source = context.existing_path(source)?;
    let document = read_document(context, &source)?;
    let destination_snapshot = context.capture_destination(destination)?;
    let destination = destination_snapshot.path().to_path_buf();
    let destination_assets = asset::document_asset_directory(&destination)?;
    context.destination_path(&destination_assets)?;
    let _save_as_guard = asset::lock_save_as_destination(&destination)?;
    let mut destination_document = if destination.exists() {
        Some(read_document(context, &destination)?)
    } else {
        None
    };
    let mut destination_revision: Option<DiskRevision> = destination_document
        .as_ref()
        .and_then(|value| value.revision.clone())
        .map(|revision| revision.to_disk_revision())
        .transpose()?;
    if expected_destination_hash.is_some() && destination_revision.is_none() {
        return Err(ApiError::new(
            "revision_conflict",
            "The expected destination no longer exists.",
        ));
    }
    if destination_revision.is_some() && !force && expected_destination_hash.is_none() {
        return Err(ApiError::new(
            "confirmation_required",
            "Replacing the destination requires --expected-destination-hash or --force.",
        ));
    }
    if let (Some(expected), Some(actual)) =
        (expected_destination_hash, destination_revision.as_ref())
    {
        if expected != actual.hash {
            return Err(ApiError::new(
                "revision_conflict",
                "The destination hash has changed.",
            ));
        }
    }
    // Resolve every resource and validate the final text encoding before any
    // destination asset is created. The tracked copy below is rolled back if
    // the guarded document write fails or detects a concurrent change.
    let preview_plan = asset::preview_referenced_assets_for_save_as_plan(
        &source,
        &destination,
        &document.content,
        context.root.as_deref(),
    )?;
    let preview = preview_plan.content();
    let preview_bytes =
        encoding::encode(preview, &document.encoding, &document.eol, document.had_bom)?;
    let previous_content = destination_document
        .as_ref()
        .map(|value| value.content.as_str())
        .unwrap_or("");
    if dry_run {
        let content_hash = blake3::hash(preview.as_bytes()).to_hex().to_string();
        let encoded_hash = blake3::hash(&preview_bytes).to_hex().to_string();
        let document_changed = destination_revision
            .as_ref()
            .is_none_or(|revision| revision.hash != encoded_hash);
        let changed = document_changed || preview_plan.requires_copy();
        let changed_ranges = changed_ranges(previous_content, preview);
        return Ok(DocumentMutationOutcome {
            path: destination.to_string_lossy().into_owned(),
            changed,
            dry_run: true,
            previous_revision: destination_revision.map(Into::into),
            revision: None,
            content_hash,
            operations: Vec::new(),
            changed_ranges,
            diff: (previous_content != preview)
                .then(|| diff_preview(&destination, previous_content, preview)),
        });
    }

    let _destination_guard = context.revalidate_destination(&destination_snapshot)?;
    let copied_assets = asset::copy_referenced_assets_for_save_as_tracked(
        &source,
        &destination,
        &document.content,
        context.root.as_deref(),
    )?;
    let assets_changed = copied_assets.created_any();
    let rewritten = copied_assets.content();
    // A concurrent asset creation can select a collision-safe name that was
    // not present during preview. Re-encode only in that rare case.
    let bytes = if rewritten == preview {
        preview_bytes
    } else {
        encoding::encode(
            rewritten,
            &document.encoding,
            &document.eol,
            document.had_bom,
        )?
    };
    let encoded_hash = blake3::hash(&bytes).to_hex().to_string();
    let initially_unchanged = destination_revision
        .as_ref()
        .is_some_and(|revision| revision.hash == encoded_hash);
    if initially_unchanged {
        let refreshed_document = if destination.is_file() {
            match read_document(context, &destination) {
                Ok(document) => Some(document),
                Err(_) if !destination.exists() => None,
                Err(error) => return Err(error),
            }
        } else if destination.exists() {
            return Err(conflict_error(None));
        } else {
            None
        };
        let refreshed_revision = refreshed_document
            .as_ref()
            .and_then(|value| value.revision.clone())
            .map(|revision| revision.to_disk_revision())
            .transpose()?;
        if refreshed_revision != destination_revision {
            if !force {
                return Err(conflict_error(refreshed_revision));
            }
            destination_document = refreshed_document;
            destination_revision = refreshed_revision;
        }
    }
    let content_hash = blake3::hash(rewritten.as_bytes()).to_hex().to_string();
    let document_changed = destination_revision
        .as_ref()
        .is_none_or(|revision| revision.hash != encoded_hash);
    let previous_content = destination_document
        .as_ref()
        .map(|value| value.content.as_str())
        .unwrap_or("");
    let changed_ranges = changed_ranges(previous_content, rewritten);
    if !document_changed {
        let revision = destination_revision.clone().map(Into::into);
        let _rewritten = copied_assets.commit();
        return Ok(DocumentMutationOutcome {
            path: destination.to_string_lossy().into_owned(),
            changed: assets_changed,
            dry_run: false,
            previous_revision: revision.clone(),
            revision,
            content_hash,
            operations: Vec::new(),
            changed_ranges: Vec::new(),
            diff: None,
        });
    }
    if let Some(destination_document) = destination_document.as_ref() {
        checkpoint_previous(context, destination_document)?;
    }
    let outcome = match destination_revision.as_ref() {
        Some(_) if force => atomic_replace_existing(&destination, &bytes)?,
        Some(expected) => atomic_write_if_revision(&destination, &bytes, Some(expected))?,
        None => atomic_create_if_absent(&destination, &bytes)?,
    };
    if let AtomicWriteOutcome::Conflict(current) = outcome {
        return Err(conflict_error(current));
    }
    // The document now references these files, so they must survive even if a
    // later metadata read fails.
    let _rewritten = copied_assets.commit();
    let updated = revision_from_bytes(&destination, &bytes)?;
    Ok(DocumentMutationOutcome {
        path: destination.to_string_lossy().into_owned(),
        changed: true,
        dry_run: false,
        previous_revision: destination_revision.map(Into::into),
        revision: Some(updated.into()),
        content_hash,
        operations: Vec::new(),
        changed_ranges,
        diff: None,
    })
}

fn changed_ranges(before: &str, after: &str) -> Vec<TextRange> {
    if before == after {
        return Vec::new();
    }
    let (start, before_end, _) = change_bounds(before, after);
    vec![TextRange {
        start: position_at(before, start),
        end: position_at(before, before_end),
    }]
}

fn change_bounds(before: &str, after: &str) -> (usize, usize, usize) {
    let mut prefix = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while prefix > 0 && (!before.is_char_boundary(prefix) || !after.is_char_boundary(prefix)) {
        prefix -= 1;
    }

    let before_tail = &before[prefix..];
    let after_tail = &after[prefix..];
    let mut suffix = before_tail
        .bytes()
        .rev()
        .zip(after_tail.bytes().rev())
        .take_while(|(left, right)| left == right)
        .count();
    while suffix > 0
        && (!before.is_char_boundary(before.len() - suffix)
            || !after.is_char_boundary(after.len() - suffix))
    {
        suffix -= 1;
    }
    (prefix, before.len() - suffix, after.len() - suffix)
}

fn position_at(content: &str, byte_offset: usize) -> TextPosition {
    let prefix = &content[..byte_offset];
    TextPosition {
        line: prefix.bytes().filter(|value| *value == b'\n').count() + 1,
        column: prefix
            .rsplit_once('\n')
            .map(|(_, tail)| tail.chars().count() + 1)
            .unwrap_or_else(|| prefix.chars().count() + 1),
    }
}

fn diff_preview(path: &Path, before: &str, after: &str) -> String {
    let (start, before_end, after_end) = change_bounds(before, after);
    let old_range = TextRange {
        start: position_at(before, start),
        end: position_at(before, before_end),
    };
    let new_start = position_at(after, start);
    let new_end = position_at(after, after_end);
    let render = |marker: char, value: &str| {
        let truncated = value.chars().count() > 2_000;
        let mut rendered = value
            .chars()
            .take(2_000)
            .collect::<String>()
            .split('\n')
            .map(|line| format!("{marker}{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        if truncated {
            rendered.push_str(&format!("\n{marker}… (diff truncated)"));
        }
        rendered
    };
    format!(
        "--- {} (current)\n+++ {} (proposed)\n@@ {}:{}-{}:{} -> {}:{}-{}:{} @@\n{}\n{}",
        path.display(),
        path.display(),
        old_range.start.line,
        old_range.start.column,
        old_range.end.line,
        old_range.end.column,
        new_start.line,
        new_start.column,
        new_end.line,
        new_end.column,
        render('-', &before[start..before_end]),
        render('+', &after[start..after_end]),
    )
}

pub fn add_asset(
    context: &CliContext,
    document_path: Option<&Path>,
    document_id: Option<&str>,
    source_path: Option<&Path>,
    data_base64: Option<String>,
    mime_type: Option<String>,
) -> ApiResult<WriteAssetResult> {
    let path_lock = lock_path_mutations()?;
    let request = prepare_asset_request(
        context,
        document_path,
        document_id,
        source_path,
        data_base64,
        mime_type,
    )?;
    let destination = request
        .document_path
        .as_deref()
        .map(Path::new)
        .map(asset::document_asset_directory)
        .transpose()?
        .map(|path| context.capture_destination(&path))
        .transpose()?;
    let _destination_guard = destination
        .as_ref()
        .map(|snapshot| context.revalidate_destination(snapshot))
        .transpose()?;
    asset::write_asset_locked(&context.data_dir.join("Recovery"), request, &path_lock)
}

pub fn preview_asset(
    context: &CliContext,
    document_path: Option<&Path>,
    document_id: Option<&str>,
    source_path: Option<&Path>,
    data_base64: Option<String>,
    mime_type: Option<String>,
) -> ApiResult<WriteAssetResult> {
    let request = prepare_asset_request(
        context,
        document_path,
        document_id,
        source_path,
        data_base64,
        mime_type,
    )?;
    asset::preview_asset(&context.data_dir.join("Recovery"), request)
}

fn prepare_asset_request(
    context: &CliContext,
    document_path: Option<&Path>,
    document_id: Option<&str>,
    source_path: Option<&Path>,
    data_base64: Option<String>,
    mime_type: Option<String>,
) -> ApiResult<WriteAssetRequest> {
    let document_path = document_path
        .map(|path| context.existing_file(path))
        .transpose()?;
    if let Some(document_path) = document_path.as_deref() {
        let asset_directory = asset::document_asset_directory(document_path)?;
        context.destination_path(&asset_directory)?;
    }
    let source_path = source_path
        .map(|path| context.existing_path(path))
        .transpose()?;
    if context.root.is_some() && document_path.is_none() {
        return Err(ApiError::new(
            "path_outside_workspace",
            "Unsaved assets cannot be written through --root. Provide --document inside the root.",
        ));
    }
    let document_id = document_id
        .map(str::to_string)
        .or_else(|| {
            document_path.as_ref().map(|path| {
                blake3::hash(path.to_string_lossy().as_bytes()).to_hex()[..32].to_string()
            })
        })
        .ok_or_else(|| {
            ApiError::new(
                "missing_document",
                "Provide --document or --document-id for an unsaved asset.",
            )
        })?;
    Ok(WriteAssetRequest {
        document_id,
        document_path: document_path.map(|path| path.to_string_lossy().into_owned()),
        source_path: source_path.map(|path| path.to_string_lossy().into_owned()),
        data_base64,
        mime_type,
    })
}

fn checkpoint_previous(context: &CliContext, document: &DocumentInfo) -> ApiResult<()> {
    let recovery = context.recovery()?;
    let document_id = document
        .path
        .as_ref()
        .map(|path| blake3::hash(path.as_bytes()).to_hex()[..32].to_string())
        .unwrap_or_else(|| "cli-document".into());
    let _ = recovery.checkpoint(CheckpointRequest {
        document_id,
        path: document.path.clone(),
        title: document.title.clone(),
        content: document.content.clone(),
        kind: Some("history".into()),
    })?;
    Ok(())
}

fn conflict_error(current: Option<DiskRevision>) -> ApiError {
    let detail = current
        .map(|value| format!(" Current hash: {}.", value.hash))
        .unwrap_or_else(|| " The destination no longer exists.".into());
    ApiError::new(
        "revision_conflict",
        format!("The file changed before InkFlow could commit the update.{detail}"),
    )
}

fn document_stats(content: &str) -> DocumentStats {
    let chinese = content
        .chars()
        .filter(|character| matches!(*character as u32, 0x3400..=0x9fff))
        .count();
    let latin_pattern =
        Regex::new(r"[\p{L}\p{N}]+(?:['’_-][\p{L}\p{N}]+)*").expect("valid word pattern");
    let without_chinese: String = content
        .chars()
        .map(|character| {
            if matches!(character as u32, 0x3400..=0x9fff) {
                ' '
            } else {
                character
            }
        })
        .collect();
    DocumentStats {
        words: chinese + latin_pattern.find_iter(&without_chinese).count(),
        lines: if content.is_empty() {
            1
        } else {
            content.lines().count() + usize::from(content.ends_with('\n'))
        },
        characters: content.chars().count(),
    }
}

fn extract_outline(content: &str) -> Vec<OutlineItem> {
    let newline_offsets = content
        .bytes()
        .enumerate()
        .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset))
        .collect::<Vec<_>>();
    let mut result = Vec::new();
    let mut active_heading: Option<(u8, usize, String)> = None;
    for (event, range) in Parser::new_ext(content, Options::all()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                active_heading = Some((heading_level(level), range.start, String::new()));
            }
            Event::Text(text)
            | Event::Code(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => {
                if let Some((_, _, heading)) = active_heading.as_mut() {
                    heading.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, _, heading)) = active_heading.as_mut() {
                    heading.push(' ');
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, offset, text)) = active_heading.take() {
                    result.push(OutlineItem {
                        level,
                        text,
                        line: newline_offsets.partition_point(|newline| *newline < offset) + 1,
                    });
                }
            }
            _ => {}
        }
    }
    result
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn has_remote_images(content: &str) -> bool {
    let parser = Parser::new_ext(content, Options::all());
    let mut mermaid_source: Option<String> = None;
    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info
                    .split_whitespace()
                    .next()
                    .is_some_and(|language| language.eq_ignore_ascii_case("mermaid")) =>
            {
                mermaid_source = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if mermaid_source
                    .take()
                    .is_some_and(|source| has_remote_mermaid_image_reference(&source))
                {
                    return true;
                }
            }
            Event::Text(text) if mermaid_source.is_some() => {
                mermaid_source
                    .as_mut()
                    .expect("checked Mermaid buffer")
                    .push_str(text.as_ref());
            }
            Event::SoftBreak | Event::HardBreak if mermaid_source.is_some() => {
                mermaid_source
                    .as_mut()
                    .expect("checked Mermaid buffer")
                    .push('\n');
            }
            Event::Start(Tag::Image { dest_url, .. })
                if is_remote_destination(dest_url.as_ref()) =>
            {
                return true;
            }
            Event::Html(html) | Event::InlineHtml(html) => {
                if has_remote_html_image(html.as_ref()) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn has_remote_mermaid_image_reference(source: &str) -> bool {
    let normalized = decode_mermaid_string_escapes(source);
    if has_remote_html_image(&normalized) {
        return true;
    }

    let image_property = Regex::new(
        r#"(?is)(?:^|[,\{\s])(?:img|icon|"img"|"icon"|'img'|'icon')\s*:\s*(?:"((?:\\.|[^"\\])*)"|'((?:''|[^'])*)'|([^,}\]\s]+))"#,
    )
    .expect("valid Mermaid image property pattern");
    for captures in image_property.captures_iter(&normalized) {
        let value = captures
            .get(1)
            .or_else(|| captures.get(2))
            .or_else(|| captures.get(3))
            .map(|value| value.as_str())
            .unwrap_or("");
        if mermaid_value_is_remote(&normalized, value) {
            return true;
        }
    }

    let markdown_image = Regex::new(r#"(?is)!\[[^\]]*\]\(\s*(?:<([^>\r\n]+)>|([^\s)\r\n]+))"#)
        .expect("valid Mermaid Markdown image pattern");
    markdown_image.captures_iter(&normalized).any(|captures| {
        captures
            .get(1)
            .or_else(|| captures.get(2))
            .is_some_and(|value| is_remote_destination(value.as_str()))
    })
}

fn has_remote_html_image(source: &str) -> bool {
    static TAG_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)<(?:img|source)\b(?:[^>\"']|\"[^\"]*\"|'[^']*')*>"#)
            .expect("valid HTML image tag pattern")
    });
    static ATTRIBUTE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?is)(?:^|\s)(src|srcset)\s*=\s*(?:\"([^\"]*)\"|'([^']*)'|([^\s\"'=<>`]+))"#)
            .expect("valid HTML image attribute pattern")
    });
    TAG_PATTERN.find_iter(source).any(|tag| {
        ATTRIBUTE_PATTERN
            .captures_iter(tag.as_str())
            .any(|captures| {
                let attribute = captures
                    .get(1)
                    .map(|value| value.as_str().to_ascii_lowercase())
                    .unwrap_or_default();
                let value = captures
                    .get(2)
                    .or_else(|| captures.get(3))
                    .or_else(|| captures.get(4))
                    .map(|value| decode_html_url_attribute(value.as_str()))
                    .unwrap_or_default();
                if attribute == "srcset" {
                    asset::srcset_path_ranges(&value)
                        .into_iter()
                        .any(|range| is_remote_destination(&value[range]))
                } else {
                    is_remote_destination(&value)
                }
            })
    })
}

fn decode_html_url_attribute(source: &str) -> String {
    let mut decoded = String::with_capacity(source.len());
    let mut remaining = source;
    while let Some(entity_start) = remaining.find('&') {
        decoded.push_str(&remaining[..entity_start]);
        remaining = &remaining[entity_start..];
        if let Some((character, consumed)) = decode_numeric_html_reference(remaining) {
            decoded.push(character);
            remaining = &remaining[consumed..];
            continue;
        }
        let Some(entity_end) = remaining.find(';').filter(|end| *end <= 16) else {
            decoded.push('&');
            remaining = &remaining[1..];
            continue;
        };
        let entity = &remaining[1..entity_end];
        let character = match entity.to_ascii_lowercase().as_str() {
            "colon" => Some(':'),
            "sol" => Some('/'),
            "bsol" => Some('\\'),
            "tab" => Some('\t'),
            "newline" => Some('\n'),
            _ => None,
        };
        if let Some(character) = character {
            decoded.push(character);
        } else {
            decoded.push_str(&remaining[..=entity_end]);
        }
        remaining = &remaining[entity_end + 1..];
    }
    decoded.push_str(remaining);
    decoded
}

fn decode_numeric_html_reference(source: &str) -> Option<(char, usize)> {
    let (radix, digits_start) = if source.starts_with("&#x") || source.starts_with("&#X") {
        (16, 3)
    } else if source.starts_with("&#") {
        (10, 2)
    } else {
        return None;
    };
    let digits_len = source[digits_start..]
        .bytes()
        .take_while(|byte| match radix {
            16 => byte.is_ascii_hexdigit(),
            _ => byte.is_ascii_digit(),
        })
        .count();
    if digits_len == 0 {
        return None;
    }
    let digits_end = digits_start + digits_len;
    let value = u32::from_str_radix(&source[digits_start..digits_end], radix).ok()?;
    let character = char::from_u32(value)?;
    let consumed = digits_end + usize::from(source.as_bytes().get(digits_end) == Some(&b';'));
    Some((character, consumed))
}

fn mermaid_value_is_remote(source: &str, value: &str) -> bool {
    let value = normalize_mermaid_scalar(value);
    if is_remote_destination(&value) {
        return true;
    }
    let Some(anchor) = value.strip_prefix('*') else {
        return false;
    };
    if anchor.is_empty()
        || !anchor
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return false;
    }
    let anchor_pattern = Regex::new(&format!(
        r#"(?is)&{}\s+(?:"((?:\\.|[^"\\])*)"|'((?:''|[^'])*)'|([^\s,]+))"#,
        regex::escape(anchor)
    ))
    .expect("escaped Mermaid anchor pattern");
    anchor_pattern.captures(source).is_some_and(|captures| {
        let anchored = captures
            .get(1)
            .or_else(|| captures.get(2))
            .or_else(|| captures.get(3))
            .map(|value| value.as_str().trim_end_matches(['}', ']']))
            .unwrap_or("");
        is_remote_destination(&normalize_mermaid_scalar(anchored))
    })
}

fn normalize_mermaid_scalar(source: &str) -> String {
    let decoded = decode_mermaid_string_escapes(source);
    let bytes = decoded.as_bytes();
    let mut result = String::with_capacity(decoded.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' {
            let newline_bytes = if bytes.get(index + 1) == Some(&b'\n') {
                2
            } else if bytes.get(index + 1) == Some(&b'\r') && bytes.get(index + 2) == Some(&b'\n') {
                3
            } else {
                0
            };
            if newline_bytes > 0 {
                index += newline_bytes;
                while matches!(bytes.get(index), Some(b' ' | b'\t')) {
                    index += 1;
                }
                continue;
            }
        }
        let character = decoded[index..]
            .chars()
            .next()
            .expect("index remains at a character boundary");
        result.push(character);
        index += character.len_utf8();
    }
    result
}

fn decode_mermaid_string_escapes(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut result = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape_length = if bytes.get(index) == Some(&b'\\') {
            match bytes.get(index + 1) {
                Some(b'x') => Some(2),
                Some(b'u') => Some(4),
                Some(b'U') => Some(8),
                _ => None,
            }
        } else {
            None
        };
        if let Some(length) = escape_length {
            let start = index + 2;
            let end = start + length;
            if end <= bytes.len()
                && bytes[start..end]
                    .iter()
                    .all(|value| value.is_ascii_hexdigit())
                && let Ok(hex) = std::str::from_utf8(&bytes[start..end])
                && let Ok(code_point) = u32::from_str_radix(hex, 16)
                && let Some(character) = char::from_u32(code_point)
            {
                result.push(character);
                index = end;
                continue;
            }
        }
        let character = source[index..]
            .chars()
            .next()
            .expect("index remains at a character boundary");
        result.push(character);
        index += character.len_utf8();
    }
    result
}

fn is_remote_destination(value: &str) -> bool {
    let normalized = value
        // Match the WHATWG URL parser and the shared WebView renderer: leading
        // and trailing C0 controls/spaces are removed before the scheme is read.
        .trim_matches(|character| character <= '\u{0020}')
        .chars()
        .filter(|character| !matches!(character, '\t' | '\n' | '\r'))
        .map(|character| if character == '\\' { '/' } else { character })
        .collect::<String>()
        .to_ascii_lowercase();
    normalized.starts_with("http:")
        || normalized.starts_with("https:")
        || normalized.starts_with("//")
}

pub(super) fn resolve_from_current_directory(path: &Path) -> ApiResult<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|error| ApiError::io("Unable to resolve the current directory", error))
}

pub(super) fn reject_reparse_path(candidate: &Path) -> ApiResult<()> {
    let mut current = PathBuf::new();
    for component in candidate.components() {
        current.push(component);
        if !current.exists() {
            break;
        }
        if is_symbolic_link_or_junction(&current)? {
            return Err(ApiError::new(
                "reparse_point_blocked",
                "Symbolic links and directory junctions are not followed inside --root.",
            ));
        }
    }
    Ok(())
}

fn reject_reparse_points(root: &Path, candidate: &Path) -> ApiResult<()> {
    let mut current = root.to_path_buf();
    let relative = candidate.strip_prefix(root).unwrap_or(Path::new(""));
    for component in relative.components() {
        current.push(component);
        if !current.exists() {
            break;
        }
        if is_symbolic_link_or_junction(&current)? {
            return Err(ApiError::new(
                "reparse_point_blocked",
                "Symbolic links and directory junctions are not followed inside --root.",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_missing_path_remains_accessible_only_inside_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let removed_parent = root.join("removed").join("nested");
        let document = removed_parent.join("note.md");
        fs::create_dir_all(&removed_parent).unwrap();
        fs::write(&document, "recover me").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), Some(root.clone())).unwrap();
        fs::remove_dir_all(root.join("removed")).unwrap();

        assert_eq!(
            context.scoped_path_allow_missing(&document).unwrap(),
            canonical_existing(&root)
                .unwrap()
                .join("removed")
                .join("nested")
                .join("note.md")
        );

        let outside = temp.path().join("outside").join("note.md");
        let error = context.scoped_path_allow_missing(&outside).unwrap_err();
        assert_eq!(error.code, "path_outside_workspace");

        let traversal = root
            .join("missing")
            .join("..")
            .join("..")
            .join("outside.md");
        let error = context.scoped_path_allow_missing(&traversal).unwrap_err();
        assert_eq!(error.code, "path_outside_workspace");
    }

    #[cfg(target_os = "windows")]
    fn assert_waits_for_path_mutation_lock<T, F>(operation: F) -> T
    where
        T: Send + 'static,
        F: FnOnce() -> ApiResult<T> + Send + 'static,
    {
        use std::{sync::mpsc, thread, time::Duration};

        let first = lock_path_mutations().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            finished_tx.send(operation()).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "document mutation escaped the workspace path mutation lock"
        );
        drop(first);
        let result = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        worker.join().unwrap();
        result
    }

    #[test]
    fn analysis_matches_chinese_and_latin_word_rules() {
        let result = analyze_document("# 标题\n\nInkFlow editor");
        assert_eq!(result.stats.words, 4);
        assert_eq!(result.outline[0].text, "标题");
    }

    #[test]
    fn adding_a_second_asset_revalidates_the_existing_asset_directory() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, "# Note\n").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();

        let first = add_asset(
            &context,
            Some(&document),
            None,
            None,
            Some("aW1hZ2Utb25l".into()),
            Some("image/png".into()),
        )
        .unwrap();
        let second = add_asset(
            &context,
            Some(&document),
            None,
            None,
            Some("aW1hZ2UtdHdv".into()),
            Some("image/png".into()),
        )
        .unwrap();

        assert_ne!(first.absolute_path, second.absolute_path);
        assert!(Path::new(&first.absolute_path).is_file());
        assert!(Path::new(&second.absolute_path).is_file());
    }

    #[test]
    fn outline_only_closes_fences_with_matching_markers_and_lengths() {
        let result =
            analyze_document("````text\n~~~\n# fenced one\n```\n# fenced two\n````\n# visible\n");

        assert_eq!(result.outline.len(), 1);
        assert_eq!(result.outline[0].text, "visible");
        assert_eq!(result.outline[0].line, 7);
    }

    #[test]
    fn outline_follows_commonmark_heading_rules() {
        let result = analyze_document("# C#\n\n  ## Indented\n\nSetext title\n============\n");

        assert_eq!(result.outline.len(), 3);
        assert_eq!(result.outline[0].level, 1);
        assert_eq!(result.outline[0].text, "C#");
        assert_eq!(result.outline[0].line, 1);
        assert_eq!(result.outline[1].level, 2);
        assert_eq!(result.outline[1].text, "Indented");
        assert_eq!(result.outline[1].line, 3);
        assert_eq!(result.outline[2].level, 1);
        assert_eq!(result.outline[2].text, "Setext title");
        assert_eq!(result.outline[2].line, 5);
    }

    #[test]
    fn detects_remote_markdown_images_but_not_links() {
        assert!(analyze_document("![x](https://example.com/x.png)").has_remote_images);
        assert!(!analyze_document("[x](https://example.com)").has_remote_images);
    }

    #[test]
    fn detects_remote_candidates_later_in_html_srcset() {
        let html = r#"<picture><source srcset="local.png 1x, //example.com/a.png 2x"><img src="local.png"></picture>"#;
        let data_url_with_remote_text =
            r#"<img srcset="data:image/svg+xml,https://example.com/not-a-fetch 1x">"#;
        let remote_after_data_url =
            r#"<img srcset="data:image/svg+xml,%3Csvg%3E 1x, https://example.com/a.png 2x">"#;
        let escaped_scheme = r#"<img src="h&#116;tps&colon;&sol;&sol;example.com/a.png">"#;
        let semicolonless_decimal = r#"<img src="https&#58//example.com/a.png">"#;
        let semicolonless_hex = r#"<img src="https&#x3a//example.com/a.png">"#;
        let c0_prefixed = "<img src=\"\u{0001}https://example.com/a.png\">";

        assert!(analyze_document(html).has_remote_images);
        assert!(!analyze_document(data_url_with_remote_text).has_remote_images);
        assert!(analyze_document(remote_after_data_url).has_remote_images);
        assert!(analyze_document(escaped_scheme).has_remote_images);
        assert!(analyze_document(semicolonless_decimal).has_remote_images);
        assert!(analyze_document(semicolonless_hex).has_remote_images);
        assert!(analyze_document(c0_prefixed).has_remote_images);
    }

    #[test]
    fn detects_escaped_and_multiline_mermaid_image_metadata() {
        let escaped_key = r#"```mermaid
flowchart LR
A@{ "\u0069mg": "https://example.com/a.png" }
```"#;
        let continued_value = r#"```mermaid
flowchart LR
A@{
  img: "ht\
    tps://example.com/a.png"
}
```"#;

        assert!(analyze_document(escaped_key).has_remote_images);
        assert!(analyze_document(continued_value).has_remote_images);
        assert!(has_remote_mermaid_image_reference(
            r#"A@{ source: &remote "https://example.com/a.png", img: *remote }"#
        ));
        assert!(
            !analyze_document("```mermaid\nclick A \"https://example.com\"\n```").has_remote_images
        );
    }

    #[test]
    fn write_dry_run_reports_encoding_only_byte_changes() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, b"line one\nline two\n").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();

        let outcome = write_document(
            &context,
            &document,
            WriteOptions {
                content: "line one\nline two\n",
                expected_hash: None,
                force: true,
                create: false,
                encoding: None,
                eol: Some("crlf"),
                bom: None,
                dry_run: true,
            },
        )
        .unwrap();

        assert!(outcome.changed);
        assert!(outcome.changed_ranges.is_empty());
        assert_eq!(fs::read(&document).unwrap(), b"line one\nline two\n");
    }

    #[test]
    fn write_normalizes_input_before_hashing_and_skips_identical_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, b"line one\r\nline two\r\n").unwrap();
        let data = temp.path().join("data");
        let context = CliContext::new(Some(data.clone()), None).unwrap();

        let outcome = write_document(
            &context,
            &document,
            WriteOptions {
                content: "line one\r\nline two\r\n",
                expected_hash: None,
                force: true,
                create: false,
                encoding: None,
                eol: None,
                bom: None,
                dry_run: false,
            },
        )
        .unwrap();

        assert!(!outcome.changed);
        assert!(outcome.changed_ranges.is_empty());
        assert_eq!(
            outcome.content_hash,
            blake3::hash(b"line one\nline two\n").to_hex().to_string()
        );
        assert_eq!(fs::read(&document).unwrap(), b"line one\r\nline two\r\n");
        assert!(!data.join("Recovery").exists());
    }

    #[test]
    fn save_as_skips_an_identical_destination_without_creating_history() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.md");
        let destination = temp.path().join("destination.md");
        let data = temp.path().join("data");
        fs::write(&source, b"same content\n").unwrap();
        fs::write(&destination, b"same content\n").unwrap();
        let context = CliContext::new(Some(data.clone()), None).unwrap();
        let before = revision(&destination).unwrap();

        let dry_run = save_document_as(&context, &source, &destination, None, true, true).unwrap();
        assert!(!dry_run.changed);
        assert!(dry_run.changed_ranges.is_empty());
        assert!(dry_run.revision.is_none());

        let committed =
            save_document_as(&context, &source, &destination, None, true, false).unwrap();
        assert!(!committed.changed);
        assert!(committed.changed_ranges.is_empty());
        assert_eq!(revision(&destination).unwrap(), before);
        assert_eq!(fs::read(&destination).unwrap(), b"same content\n");
        assert!(!data.join("Recovery").exists());
    }

    #[test]
    fn save_as_reports_and_copies_a_missing_asset_without_rewriting_identical_markdown() {
        let temp = tempfile::tempdir().unwrap();
        let source_directory = temp.path().join("source");
        let destination_directory = temp.path().join("destination");
        let source = source_directory.join("copy.md");
        let destination = destination_directory.join("copy.md");
        let source_asset = source_directory.join("copy.assets/image.png");
        let destination_asset = destination_directory.join("copy.assets/image.png");
        let data = temp.path().join("data");
        fs::create_dir_all(source_asset.parent().unwrap()).unwrap();
        fs::create_dir_all(&destination_directory).unwrap();
        fs::write(&source_asset, b"image").unwrap();
        fs::write(&source, b"![image](copy.assets/image.png)\n").unwrap();
        fs::write(&destination, b"![image](copy.assets/image.png)\n").unwrap();
        let context = CliContext::new(Some(data.clone()), None).unwrap();
        let before = revision(&destination).unwrap();

        let dry_run = save_document_as(&context, &source, &destination, None, true, true).unwrap();
        assert!(dry_run.changed);
        assert!(dry_run.changed_ranges.is_empty());
        assert!(!destination_asset.exists());

        let committed =
            save_document_as(&context, &source, &destination, None, true, false).unwrap();
        assert!(committed.changed);
        assert!(committed.changed_ranges.is_empty());
        assert_eq!(revision(&destination).unwrap(), before);
        assert_eq!(fs::read(destination_asset).unwrap(), b"image");
        assert!(!data.join("Recovery").exists());
    }

    #[test]
    fn unchanged_write_detects_a_revision_change_before_returning_success() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, b"original\n").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let expected_hash = blake3::hash(b"original\n").to_hex().to_string();

        let error = write_document_with_hook(
            &context,
            &document,
            WriteOptions {
                content: "original\n",
                expected_hash: Some(&expected_hash),
                force: false,
                create: false,
                encoding: None,
                eol: None,
                bom: None,
                dry_run: false,
            },
            || {
                fs::write(&document, b"concurrent\n").unwrap();
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(fs::read(&document).unwrap(), b"concurrent\n");
    }

    #[test]
    fn unchanged_mutation_detects_a_revision_change_before_returning_success() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, b"original\n").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let opened = read_document(&context, &document).unwrap();

        fs::write(&document, b"concurrent\n").unwrap();
        let error = save_mutation(&context, &opened, opened.content.clone(), Vec::new(), false)
            .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(fs::read(&document).unwrap(), b"concurrent\n");
    }

    #[test]
    fn unchanged_mutation_detects_a_deleted_document() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, b"original\n").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let opened = read_document(&context, &document).unwrap();

        fs::remove_file(&document).unwrap();
        let error = save_mutation(&context, &opened, opened.content.clone(), Vec::new(), false)
            .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert!(!document.exists());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn changed_mutation_rejects_a_replaced_parent_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let documents = root.join("documents");
        let moved_documents = root.join("moved-documents");
        fs::create_dir_all(&documents).unwrap();
        let document = documents.join("note.md");
        fs::write(&document, b"original\n").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), Some(root.clone())).unwrap();
        let opened = read_document(&context, &document).unwrap();
        let replacement_documents = documents.clone();
        let replacement_target = document.clone();

        let error = save_mutation_with_hook(
            &context,
            &opened,
            "edited\n".into(),
            Vec::new(),
            false,
            || {
                fs::rename(&documents, &moved_documents).unwrap();
                fs::create_dir(&replacement_documents).unwrap();
                fs::write(&replacement_target, b"replacement\n").unwrap();
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "path_changed");
        assert_eq!(
            fs::read(moved_documents.join("note.md")).unwrap(),
            b"original\n"
        );
        assert_eq!(fs::read(document).unwrap(), b"replacement\n");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn forced_write_rejects_a_replaced_parent_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let documents = root.join("documents");
        let moved_documents = root.join("moved-documents");
        fs::create_dir_all(&documents).unwrap();
        let document = documents.join("note.md");
        fs::write(&document, b"original\n").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), Some(root.clone())).unwrap();
        let replacement_documents = documents.clone();
        let replacement_target = document.clone();

        let error = write_document_with_hook(
            &context,
            &document,
            WriteOptions {
                content: "original\n",
                expected_hash: None,
                force: true,
                create: false,
                encoding: None,
                eol: None,
                bom: None,
                dry_run: false,
            },
            || {
                fs::rename(&documents, &moved_documents).unwrap();
                fs::create_dir(&replacement_documents).unwrap();
                fs::write(&replacement_target, b"replacement\n").unwrap();
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error.code, "path_changed");
        assert_eq!(
            fs::read(moved_documents.join("note.md")).unwrap(),
            b"original\n"
        );
        assert_eq!(fs::read(document).unwrap(), b"replacement\n");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn cli_document_transactions_wait_for_workspace_path_mutations() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");

        let edited_path = temp.path().join("edited.md");
        fs::write(&edited_path, "original").unwrap();
        let edit_context = CliContext::new(Some(data.join("edit")), None).unwrap();
        let opened = read_document(&edit_context, &edited_path).unwrap();
        let mutation = assert_waits_for_path_mutation_lock(move || {
            save_mutation(&edit_context, &opened, "edited".into(), Vec::new(), false)
        });
        assert!(mutation.changed);
        assert_eq!(fs::read_to_string(&edited_path).unwrap(), "edited");

        let written_path = temp.path().join("written.md");
        let write_context = CliContext::new(Some(data.join("write")), None).unwrap();
        let worker_written_path = written_path.clone();
        let write = assert_waits_for_path_mutation_lock(move || {
            write_document(
                &write_context,
                &worker_written_path,
                WriteOptions {
                    content: "created",
                    expected_hash: None,
                    force: false,
                    create: true,
                    encoding: None,
                    eol: None,
                    bom: None,
                    dry_run: false,
                },
            )
        });
        assert!(write.changed);
        assert_eq!(fs::read_to_string(&written_path).unwrap(), "created");

        let source = temp.path().join("source.md");
        let destination = temp.path().join("destination.md");
        fs::write(&source, "source").unwrap();
        let save_as_context = CliContext::new(Some(data.join("save-as")), None).unwrap();
        let worker_source = source.clone();
        let worker_destination = destination.clone();
        let save_as = assert_waits_for_path_mutation_lock(move || {
            save_document_as(
                &save_as_context,
                &worker_source,
                &worker_destination,
                None,
                false,
                false,
            )
        });
        assert!(save_as.changed);
        assert_eq!(fs::read_to_string(destination).unwrap(), "source");
    }

    #[test]
    fn forced_unchanged_write_commits_after_a_concurrent_change() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("note.md");
        fs::write(&document, b"original\n").unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let concurrent_hash = blake3::hash(b"concurrent\n").to_hex().to_string();

        let outcome = write_document_with_hook(
            &context,
            &document,
            WriteOptions {
                content: "original\n",
                expected_hash: None,
                force: true,
                create: false,
                encoding: None,
                eol: None,
                bom: None,
                dry_run: false,
            },
            || {
                fs::write(&document, b"concurrent\n").unwrap();
                Ok(())
            },
        )
        .unwrap();

        assert!(outcome.changed);
        assert_eq!(outcome.previous_revision.unwrap().hash, concurrent_hash);
        assert_eq!(fs::read(&document).unwrap(), b"original\n");
    }
}
