use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use uuid::Uuid;

use crate::{
    asset::{
        apply_asset_path_rewrites, asset_path_rewrites, asset_reference_manifest,
        cleanup_pending_assets, copy_referenced_assets_for_save_as_tracked,
        has_pending_asset_references, lock_pending_assets, lock_save_as_destination,
        migrate_pending_assets_tracked,
    },
    data_lock::lock_path_mutations,
    destination::DestinationSnapshot,
    encoding,
    error::{ApiError, ApiResult},
    fileio::{
        AtomicWriteOutcome, atomic_create_if_absent, atomic_write_if_revision, canonical_existing,
        revision, revision_from_bytes, revision_metadata,
    },
    model::{
        CheckpointRequest, DiskRevision, DocumentSnapshot, ExternalChange, RecoveryWarning,
        SaveDocumentRequest, SaveOutcome,
    },
    recovery::RecoveryStore,
};

#[derive(Debug, Clone)]
struct DocumentMeta {
    id: String,
    path: PathBuf,
    revision: DiskRevision,
    observed_revision: DiskRevision,
    content_hash: String,
    last_hash_check: Instant,
}

pub struct DocumentStore {
    documents: RwLock<HashMap<String, DocumentMeta>>,
    save_lock: Mutex<()>,
}

impl DocumentStore {
    pub fn new() -> Self {
        Self {
            documents: RwLock::new(HashMap::new()),
            save_lock: Mutex::new(()),
        }
    }

    pub fn open_paths(&self, paths: Vec<String>) -> ApiResult<Vec<DocumentSnapshot>> {
        let prepared = paths
            .into_iter()
            .map(|path| {
                let id = Uuid::new_v4().to_string();
                Self::read_path(Path::new(&path), id)
            })
            .collect::<ApiResult<Vec<_>>>()?;
        let mut documents = self.documents.write();
        let mut snapshots = Vec::with_capacity(prepared.len());
        for (snapshot, meta) in prepared {
            documents.insert(snapshot.id.clone(), meta);
            snapshots.push(snapshot);
        }
        Ok(snapshots)
    }

    #[cfg(test)]
    pub fn open_path(
        &self,
        path: &Path,
        existing_id: Option<String>,
    ) -> ApiResult<DocumentSnapshot> {
        let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let (snapshot, meta) = Self::read_path(path, id.clone())?;
        self.documents.write().insert(id, meta);
        Ok(snapshot)
    }

    fn read_path(path: &Path, id: String) -> ApiResult<(DocumentSnapshot, DocumentMeta)> {
        let path = canonical_existing(path)?;
        if !path.is_file() {
            return Err(ApiError::new(
                "not_a_file",
                "The selected path is not a file.",
            ));
        }
        validate_document_path(&path)?;
        let bytes =
            fs::read(&path).map_err(|error| ApiError::io("Unable to read the document", error))?;
        let decoded = encoding::decode(&bytes)?;
        let revision = revision_from_bytes(&path, &bytes)?;
        let metadata = fs::metadata(&path)
            .map_err(|error| ApiError::io("Unable to inspect the document", error))?;
        let title = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Untitled.md")
            .to_string();
        let content_hash = blake3::hash(decoded.content.as_bytes())
            .to_hex()
            .to_string();

        Ok((
            DocumentSnapshot {
                id: id.clone(),
                path: Some(path.to_string_lossy().into_owned()),
                title,
                content: decoded.content,
                encoding: decoded.encoding,
                eol: decoded.eol,
                had_bom: decoded.had_bom,
                had_final_newline: decoded.had_final_newline,
                read_only: metadata.permissions().readonly(),
                revision: Some(revision.clone()),
            },
            DocumentMeta {
                id,
                path,
                revision: revision.clone(),
                observed_revision: revision,
                content_hash,
                last_hash_check: Instant::now(),
            },
        ))
    }

    pub fn reload(&self, document_id: &str) -> ApiResult<DocumentSnapshot> {
        let meta = self
            .documents
            .read()
            .get(document_id)
            .cloned()
            .ok_or_else(|| ApiError::new("document_not_found", "The document is not open."))?;
        let (snapshot, replacement) = Self::read_path(&meta.path, document_id.to_string())?;
        self.install_reload(document_id, &meta, replacement)?;
        Ok(snapshot)
    }

    fn install_reload(
        &self,
        document_id: &str,
        baseline: &DocumentMeta,
        replacement: DocumentMeta,
    ) -> ApiResult<()> {
        let mut documents = self.documents.write();
        let still_current = documents.get(document_id).is_some_and(|current| {
            current.path == baseline.path && current.revision == baseline.revision
        });
        if !still_current {
            return Err(ApiError::new(
                "stale_reload",
                "The document changed or closed while it was being reloaded.",
            ));
        }
        documents.insert(document_id.to_string(), replacement);
        Ok(())
    }

    pub fn save(
        &self,
        mut request: SaveDocumentRequest,
        recovery: &RecoveryStore,
        force_path: Option<PathBuf>,
        workspace_root: Option<&Path>,
    ) -> ApiResult<SaveOutcome> {
        let _save_guard = self.save_lock.lock();
        // Keep the resolved document path stable until both the on-disk
        // revision and the in-memory metadata have been committed. Workspace
        // rename/trash and saved-asset operations use the same cross-process
        // lock, with this lock always preceding any Save As/resource lock.
        let _path_guard = lock_path_mutations()?;
        let known = self.documents.read().get(&request.id).cloned();
        let explicit_save_as = force_path.is_some();
        let path = force_path
            .or_else(|| request.path.as_ref().map(PathBuf::from))
            .or_else(|| known.as_ref().map(|value| value.path.clone()));
        let Some(path) = path else {
            return Ok(SaveOutcome::NeedsPath);
        };
        validate_document_path(&path)?;

        if !explicit_save_as && known.as_ref().is_some_and(|value| value.path != path) {
            return Err(ApiError::new(
                "path_changed",
                "The document path changed. Reload the current document or use Save As.",
            ));
        }
        // Resolve links before type checks, revision reads, and asset migration.
        // Every remaining step uses this same destination, never the alias.
        let destination = DestinationSnapshot::capture_resolved(path)?;
        let path = destination.path().to_path_buf();
        validate_document_path(&path)?;
        let path_changed = known.as_ref().is_none_or(|value| value.path != path);
        if !explicit_save_as && known.is_some() && path_changed {
            return Err(ApiError::new(
                "path_changed",
                "The resolved document path changed. Reload the current document or use Save As.",
            ));
        }
        let history_has_pending = request
            .history_image_sources
            .as_ref()
            .is_some_and(|sources| {
                sources
                    .iter()
                    .any(|source| source.starts_with("inkflow-asset://"))
            });
        let _save_as_guard = if explicit_save_as
            || path_changed
            || has_pending_asset_references(&request.content)
            || history_has_pending
        {
            Some(lock_save_as_destination(&path)?)
        } else {
            None
        };
        let conflict_was_confirmed = explicit_save_as || known.is_none();
        if !path.exists() && !conflict_was_confirmed && known.is_some() {
            return Ok(SaveOutcome::Conflict {
                path: path.to_string_lossy().into_owned(),
                disk_revision: None,
            });
        }
        let mut validated_revision = None;
        if path.exists() {
            if fs::metadata(&path)
                .map_err(|error| ApiError::io("Unable to inspect the destination", error))?
                .permissions()
                .readonly()
            {
                return Err(ApiError::new(
                    "read_only",
                    "The destination is read-only. Choose another path.",
                ));
            }
            let disk = revision(&path)?;
            if !conflict_was_confirmed {
                if request
                    .expected_revision
                    .as_ref()
                    .is_some_and(|expected| expected != &disk)
                {
                    return Ok(SaveOutcome::Conflict {
                        path: path.to_string_lossy().into_owned(),
                        disk_revision: Some(disk),
                    });
                }
                let content_hash = blake3::hash(request.content.as_bytes())
                    .to_hex()
                    .to_string();
                if known
                    .as_ref()
                    .is_some_and(|value| value.content_hash == content_hash)
                    && !history_has_pending
                {
                    cleanup_saved_draft(recovery, &request.id);
                    return Ok(SaveOutcome::Saved {
                        path: path.to_string_lossy().into_owned(),
                        revision: disk,
                        content: None,
                        recovery_warnings: Vec::new(),
                        asset_rewrites: None,
                    });
                }
            }
            validated_revision = Some(disk);
        }

        let mut recovery_warnings = Vec::new();
        if let Some(warning) = checkpoint_before_save(
            recovery,
            CheckpointRequest {
                document_id: request.id.clone(),
                // These bytes still refer to the source directory; image
                // migration and the destination write have not happened yet.
                path: known
                    .as_ref()
                    .map(|meta| meta.path.to_string_lossy().into_owned()),
                title: request.title.clone(),
                content: request.content.clone(),
                kind: Some("draft".into()),
            },
        ) {
            recovery_warnings.push(warning);
        }

        if path.exists() {
            if let Ok(previous_bytes) = fs::read(&path) {
                if let Ok(previous) = encoding::decode(&previous_bytes) {
                    if let Some(warning) = checkpoint_before_save(
                        recovery,
                        CheckpointRequest {
                            document_id: request.id.clone(),
                            path: Some(path.to_string_lossy().into_owned()),
                            title: request.title.clone(),
                            content: previous.content,
                            kind: Some("history".into()),
                        },
                    ) {
                        recovery_warnings.push(warning);
                    }
                }
            }
        }

        let original_content = request.content.clone();
        // Ordinary text saves do not need to parse all image destinations.
        let manifest = if path_changed
            || has_pending_asset_references(&request.content)
            || history_has_pending
        {
            asset_reference_manifest(
                &request.content,
                request.history_image_sources.as_deref().unwrap_or_default(),
            )
        } else {
            String::new()
        };
        let mut asset_content = manifest.clone();
        let mut copied_assets = None;
        if path_changed {
            if let Some(source) = known.as_ref().map(|value| value.path.as_path()) {
                let copy = copy_referenced_assets_for_save_as_tracked(
                    source,
                    &path,
                    &asset_content,
                    workspace_root,
                )?;
                asset_content = copy.content().to_string();
                copied_assets = Some(copy);
            }
        }
        let pending_content = asset_content.clone();
        let has_pending_assets = has_pending_asset_references(&pending_content);
        let _recovery_guard = has_pending_assets
            .then(|| recovery.guard_directory())
            .transpose()?;
        let pending_assets = has_pending_assets
            .then(|| lock_pending_assets(recovery.directory()))
            .transpose()?;
        let mut migrated_assets = None;
        if let Some(pending_assets) = pending_assets.as_ref() {
            let copy =
                migrate_pending_assets_tracked(pending_assets, &request.id, &path, &asset_content)?;
            asset_content = copy.content().to_string();
            migrated_assets = Some(copy);
        }
        let asset_rewrites = asset_path_rewrites(&manifest, &asset_content);
        request.content = apply_asset_path_rewrites(&request.content, &asset_rewrites);
        let changed_content =
            (request.content != original_content).then(|| request.content.clone());
        let bytes = encoding::encode(
            &request.content,
            &request.encoding,
            &request.eol,
            request.had_bom,
        )?;
        let _destination_guard = destination.revalidate()?;
        let write_outcome = match validated_revision.as_ref() {
            Some(expected) => atomic_write_if_revision(&path, &bytes, Some(expected))?,
            None => atomic_create_if_absent(&path, &bytes)?,
        };
        if let AtomicWriteOutcome::Conflict(disk_revision) = write_outcome {
            return Ok(SaveOutcome::Conflict {
                path: path.to_string_lossy().into_owned(),
                disk_revision,
            });
        }
        if let Some(copy) = copied_assets.take() {
            let _ = copy.commit();
        }
        if let Some(copy) = migrated_assets.take() {
            let _ = copy.commit();
        }
        let disk_revision = revision_from_bytes(&path, &bytes)?;
        let canonical = canonical_existing(&path)?;
        self.documents.write().insert(
            request.id.clone(),
            DocumentMeta {
                id: request.id.clone(),
                path: canonical.clone(),
                revision: disk_revision.clone(),
                observed_revision: disk_revision.clone(),
                content_hash: blake3::hash(request.content.as_bytes())
                    .to_hex()
                    .to_string(),
                last_hash_check: Instant::now(),
            },
        );
        if let Some(pending_assets) = pending_assets.as_ref() {
            let _ = cleanup_pending_assets(pending_assets, &request.id, &pending_content);
        }
        drop(pending_assets);
        cleanup_saved_draft(recovery, &request.id);
        Ok(SaveOutcome::Saved {
            path: canonical.to_string_lossy().into_owned(),
            revision: disk_revision,
            content: changed_content,
            recovery_warnings,
            asset_rewrites: (!asset_rewrites.is_empty()).then_some(asset_rewrites),
        })
    }

    pub fn relocate_paths(&self, source: &Path, destination: &Path, is_directory: bool) {
        let mut documents = self.documents.write();
        for meta in documents.values_mut() {
            let matches = meta.path == source || (is_directory && meta.path.starts_with(source));
            if !matches {
                continue;
            }
            let suffix = meta.path.strip_prefix(source).unwrap_or(Path::new(""));
            meta.path = destination.join(suffix);
        }
    }

    pub fn check_external_changes(&self) -> Vec<ExternalChange> {
        self.check_external_changes_with(revision)
    }

    fn check_external_changes_with(
        &self,
        read_revision: impl Fn(&Path) -> ApiResult<crate::model::DiskRevision>,
    ) -> Vec<ExternalChange> {
        let snapshots: Vec<_> = self.documents.read().values().cloned().collect();
        let mut observations = Vec::new();
        for baseline in snapshots {
            let mut observed = baseline.clone();
            let deleted = match baseline.path.try_exists() {
                Ok(exists) => !exists,
                Err(_) => continue,
            };
            if !deleted {
                let Ok((modified_ms, size)) = revision_metadata(&baseline.path) else {
                    continue;
                };
                let metadata_changed = modified_ms != baseline.observed_revision.modified_ms
                    || size != baseline.observed_revision.size;
                let hash_due = baseline.last_hash_check.elapsed() >= Duration::from_secs(60);
                if metadata_changed || hash_due {
                    let Ok(current) = read_revision(&baseline.path) else {
                        continue;
                    };
                    observed.observed_revision = current;
                    observed.last_hash_check = Instant::now();
                }
            }
            observations.push((baseline, observed, deleted));
        }
        // Validate the entire batch after every disk read. An earlier document
        // may have been saved while a later document was still being read.
        let mut changes = Vec::new();
        let mut documents = self.documents.write();
        for (baseline, observed, deleted) in observations {
            let Some(current) = documents.get_mut(&baseline.id) else {
                continue;
            };
            // Saves, reloads, relocations, and newer polls invalidate this observation.
            if current.path != baseline.path
                || current.revision != baseline.revision
                || current.observed_revision != baseline.observed_revision
                || current.last_hash_check != baseline.last_hash_check
            {
                continue;
            }
            if deleted || observed.observed_revision != baseline.revision {
                changes.push(ExternalChange {
                    document_id: baseline.id.clone(),
                    path: baseline.path.to_string_lossy().into_owned(),
                    kind: if deleted { "deleted" } else { "modified" }.into(),
                    revision: (!deleted).then(|| observed.observed_revision.clone()),
                });
            }
            *current = observed;
        }
        changes
    }

    pub fn close(&self, document_id: &str) {
        self.documents.write().remove(document_id);
    }

    pub fn path_for(&self, id: &str) -> Option<PathBuf> {
        self.documents
            .read()
            .get(id)
            .map(|value| value.path.clone())
    }
}

fn cleanup_saved_draft(recovery: &RecoveryStore, document_id: &str) {
    if let Err(error) = recovery.try_delete_document_kind(document_id, "draft") {
        eprintln!(
            "InkFlow warning: the document was saved, but its recovery draft could not be cleaned up: [{}] {}",
            error.code, error.message
        );
    }
}

fn validate_document_path(path: &Path) -> ApiResult<()> {
    if !crate::workspace::is_markdown(path) {
        return Err(ApiError::new(
            "unsupported_document_type",
            "Only Markdown files (.md, .markdown, .mdown, .mkd) can be edited and saved.",
        ));
    }
    Ok(())
}

fn checkpoint_before_save(
    recovery: &RecoveryStore,
    request: CheckpointRequest,
) -> Option<RecoveryWarning> {
    let kind = request.kind.as_deref().unwrap_or("draft").to_string();
    match recovery.try_checkpoint(request) {
        Ok(_) => None,
        Err(error) => {
            eprintln!(
                "InkFlow warning: the document will be saved without its {kind} recovery checkpoint: [{}] {}",
                error.code, error.message
            );
            Some(RecoveryWarning {
                code: error.code,
                message: error.message,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save_request(
        snapshot: &DocumentSnapshot,
        path: &Path,
        content: &str,
    ) -> SaveDocumentRequest {
        SaveDocumentRequest {
            id: snapshot.id.clone(),
            path: Some(path.to_string_lossy().into_owned()),
            title: snapshot.title.clone(),
            content: content.into(),
            encoding: snapshot.encoding.clone(),
            eol: snapshot.eol.clone(),
            had_bom: snapshot.had_bom,
            expected_revision: snapshot.revision.clone(),
            history_image_sources: None,
        }
    }

    #[test]
    fn save_as_manifest_preserves_the_shared_markdown_and_html_fixtures() {
        let fixtures: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/image-rewrites.json")).unwrap();
        for fixture in fixtures.as_array().unwrap() {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source.md");
            let target_dir = temp.path().join("B");
            fs::create_dir(&target_dir).unwrap();
            let target = target_dir.join("Copy.md");
            let content = fixture["content"].as_str().unwrap();
            fs::write(&source, content).unwrap();
            for image in fixture["assets"].as_array().unwrap() {
                let image = temp.path().join(image.as_str().unwrap());
                fs::create_dir_all(image.parent().unwrap()).unwrap();
                fs::write(image, b"image bytes").unwrap();
            }
            let store = DocumentStore::new();
            let snapshot = store.open_path(&source, None).unwrap();
            let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
            let result = store
                .save(
                    save_request(&snapshot, &target, content),
                    &recovery,
                    Some(target.clone()),
                    None,
                )
                .unwrap();
            assert!(matches!(
                result,
                SaveOutcome::Saved {
                    asset_rewrites: Some(_),
                    ..
                }
            ));
            assert_eq!(
                fs::read_to_string(target).unwrap(),
                fixture["rewritten"].as_str().unwrap(),
                "fixture: {}",
                fixture["name"]
            );
        }
    }

    #[test]
    fn save_as_migrates_history_only_images_without_changing_the_saved_content() {
        let temp = tempfile::tempdir().unwrap();
        let source_dir = temp.path().join("A");
        let target_dir = temp.path().join("B");
        fs::create_dir_all(source_dir.join("note.assets")).unwrap();
        fs::create_dir_all(target_dir.join("Copy name.assets")).unwrap();
        let source = source_dir.join("note.md");
        let target = target_dir.join("Copy name.md");
        fs::write(&source, "![x](note.assets/x.png)").unwrap();
        fs::write(source_dir.join("note.assets/x.png"), b"source image").unwrap();
        fs::write(
            target_dir.join("Copy name.assets/x.png"),
            b"different image",
        )
        .unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&source, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let mut request = save_request(&snapshot, &target, "image was deleted");
        request.history_image_sources = Some(vec!["note.assets/x.png".into()]);
        let saved = store
            .save(request, &recovery, Some(target.clone()), None)
            .unwrap();
        let SaveOutcome::Saved {
            content,
            asset_rewrites: Some(rewrites),
            ..
        } = saved
        else {
            panic!("missing history rewrite")
        };
        assert!(content.is_none());
        assert_eq!(fs::read_to_string(&target).unwrap(), "image was deleted");
        assert_eq!(rewrites.len(), 1);
        assert_eq!(rewrites[0].source, "note.assets/x.png");
        assert_eq!(
            fs::read(target_dir.join(&rewrites[0].destination)).unwrap(),
            b"source image"
        );
        assert_eq!(
            fs::read(target_dir.join("Copy name.assets/x.png")).unwrap(),
            b"different image"
        );
        assert_eq!(
            fs::read(source_dir.join("note.assets/x.png")).unwrap(),
            b"source image"
        );
    }

    #[test]
    fn first_save_migrates_pending_images_from_history_only() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let pending = recovery.directory().join("assets/draft-document");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("x.png"), b"pending image").unwrap();
        let target = temp.path().join("first.md");
        let request = SaveDocumentRequest {
            id: "draft-document".into(),
            path: Some(target.to_string_lossy().into()),
            title: "Untitled".into(),
            content: "image was undone".into(),
            encoding: "utf-8".into(),
            eol: "lf".into(),
            had_bom: false,
            expected_revision: None,
            history_image_sources: Some(vec!["inkflow-asset://x.png".into()]),
        };
        let saved = DocumentStore::new()
            .save(request, &recovery, Some(target.clone()), None)
            .unwrap();
        let SaveOutcome::Saved {
            asset_rewrites: Some(rewrites),
            ..
        } = saved
        else {
            panic!("missing pending history rewrite")
        };
        assert_eq!(rewrites[0].source, "inkflow-asset://x.png");
        assert_eq!(
            fs::read(temp.path().join(&rewrites[0].destination)).unwrap(),
            b"pending image"
        );
        assert!(!pending.join("x.png").exists());
        assert_eq!(fs::read_to_string(target).unwrap(), "image was undone");
    }

    #[test]
    fn history_asset_failure_rolls_back_all_copies_and_keeps_the_source_document() {
        let temp = tempfile::tempdir().unwrap();
        let source_dir = temp.path().join("A");
        let target_dir = temp.path().join("B");
        fs::create_dir(&source_dir).unwrap();
        fs::create_dir(&target_dir).unwrap();
        let source = source_dir.join("note.md");
        let target = target_dir.join("copy.md");
        fs::write(&source, "source").unwrap();
        fs::write(source_dir.join("x.png"), b"small image").unwrap();
        fs::File::create(source_dir.join("huge.png"))
            .unwrap()
            .set_len(50 * 1024 * 1024 + 1)
            .unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&source, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let mut request = save_request(&snapshot, &target, "![x](x.png)");
        request.history_image_sources = Some(vec!["huge.png".into()]);
        let error = store
            .save(request, &recovery, Some(target.clone()), None)
            .unwrap_err();
        assert_eq!(error.code, "resource_too_large");
        assert!(!target.exists());
        assert!(!target_dir.join("copy.assets").exists());
        assert_eq!(
            store.path_for(&snapshot.id).unwrap(),
            canonical_existing(&source).unwrap()
        );
        assert_eq!(fs::read_to_string(source).unwrap(), "source");
    }

    #[test]
    fn save_as_checks_the_type_of_symbolic_link_targets() {
        for extension in ["png", "pdf"] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source.md");
            let target = temp.path().join(format!("resource.{extension}"));
            let alias = temp.path().join("alias.md");
            fs::write(&source, "source").unwrap();
            fs::write(&target, b"original resource bytes").unwrap();
            if !test_file_symlink(&target, &alias) {
                return;
            }
            let store = DocumentStore::new();
            let snapshot = store.open_path(&source, None).unwrap();
            let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
            let error = store
                .save(
                    save_request(&snapshot, &alias, "replacement"),
                    &recovery,
                    Some(alias.clone()),
                    None,
                )
                .unwrap_err();
            assert_eq!(error.code, "unsupported_document_type");
            assert_eq!(fs::read(&target).unwrap(), b"original resource bytes");
            assert!(recovery.list().unwrap().is_empty());
            assert_eq!(
                store.path_for(&snapshot.id).unwrap(),
                canonical_existing(&source).unwrap()
            );
        }
    }

    #[test]
    fn save_as_through_a_markdown_link_uses_the_real_asset_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source_dir = temp.path().join("source");
        let target_dir = temp.path().join("target");
        fs::create_dir(&source_dir).unwrap();
        fs::create_dir(&target_dir).unwrap();
        let source = source_dir.join("note.md");
        let target = target_dir.join("real.md");
        let alias = temp.path().join("alias.md");
        fs::write(&source, "![x](image.png)").unwrap();
        fs::write(source_dir.join("image.png"), b"source image").unwrap();
        fs::write(&target, "previous").unwrap();
        if !test_file_symlink(&target, &alias) {
            return;
        }
        let store = DocumentStore::new();
        let snapshot = store.open_path(&source, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let saved = store
            .save(
                save_request(&snapshot, &alias, &snapshot.content),
                &recovery,
                Some(alias),
                None,
            )
            .unwrap();
        let SaveOutcome::Saved { path, .. } = saved else {
            panic!("not saved")
        };
        assert_eq!(Path::new(&path), canonical_existing(&target).unwrap());
        assert!(
            fs::read_to_string(&target)
                .unwrap()
                .contains("real.assets/image.png")
        );
        assert_eq!(
            fs::read(target_dir.join("real.assets/image.png")).unwrap(),
            b"source image"
        );
        assert!(!temp.path().join("alias.assets").exists());
    }

    fn test_file_symlink(target: &Path, alias: &Path) -> bool {
        #[cfg(windows)]
        let result = std::os::windows::fs::symlink_file(target, alias);
        #[cfg(not(windows))]
        let result = std::os::unix::fs::symlink(target, alias);
        if let Err(error) = &result {
            if error.raw_os_error() == Some(1314) {
                eprintln!(
                    "SKIPPED symbolic-link scenario: Windows symlink privilege is unavailable"
                );
                return false;
            }
        }
        result.unwrap();
        true
    }

    #[test]
    fn binary_resources_cannot_be_opened_or_saved_as_documents() {
        let temp = tempfile::tempdir().unwrap();
        // Valid 1x1, 24-bit BMP whose bytes also form valid UTF-8.
        let mut bmp = vec![0u8; 58];
        bmp[0..2].copy_from_slice(b"BM");
        for (offset, value) in [(2, 58u32), (10, 54), (14, 40), (18, 1), (22, 1), (34, 4)] {
            bmp[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bmp[26] = 1;
        bmp[28] = 24;
        assert!(std::str::from_utf8(&bmp).is_ok());
        let resource = temp.path().join("image.bmp");
        fs::write(&resource, &bmp).unwrap();
        let markdown = temp.path().join("note.md");
        fs::write(&markdown, "note").unwrap();
        let store = DocumentStore::new();
        let error = store
            .open_paths(vec![
                markdown.to_string_lossy().into_owned(),
                resource.to_string_lossy().into_owned(),
            ])
            .unwrap_err();
        assert_eq!(error.code, "unsupported_document_type");
        assert!(store.documents.read().is_empty());
        let snapshot = store.open_path(&markdown, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let error = store
            .save(
                save_request(&snapshot, &resource, "overwritten"),
                &recovery,
                Some(resource.clone()),
                None,
            )
            .unwrap_err();
        assert_eq!(error.code, "unsupported_document_type");
        assert_eq!(fs::read(resource).unwrap(), bmp);
        assert!(recovery.list().unwrap().is_empty());
        for extension in ["md", "MARKDOWN", "mdown", "mkd"] {
            let path = temp.path().join(format!("supported.{extension}"));
            fs::write(&path, "editable").unwrap();
            assert!(!store.open_path(&path, None).unwrap().read_only);
        }
    }

    #[test]
    fn failed_save_as_drafts_recover_images_from_the_source_directory() {
        for target_has_image in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let source_dir = temp.path().join("A");
            let target_dir = temp.path().join("B");
            fs::create_dir(&source_dir).unwrap();
            fs::create_dir(&target_dir).unwrap();
            let source = source_dir.join("note.md");
            let target = target_dir.join("note.md");
            let content = "![image](image.png)\n![large](large.png)";
            fs::write(&source, content).unwrap();
            fs::write(&target, "previous target content").unwrap();
            fs::write(source_dir.join("image.png"), b"source image").unwrap();
            if target_has_image {
                fs::write(target_dir.join("image.png"), b"wrong image").unwrap();
            }
            fs::File::create(source_dir.join("large.png"))
                .unwrap()
                .set_len(50 * 1024 * 1024 + 1)
                .unwrap();
            let store = DocumentStore::new();
            let snapshot = store.open_path(&source, None).unwrap();
            let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
            let error = store
                .save(
                    save_request(&snapshot, &target, content),
                    &recovery,
                    Some(target.clone()),
                    None,
                )
                .unwrap_err();
            assert_eq!(error.code, "resource_too_large");
            assert_eq!(
                fs::read_to_string(&target).unwrap(),
                "previous target content"
            );
            assert!(!target_dir.join("note.assets").exists());
            let entries = recovery.list().unwrap();
            let draft = entries.iter().find(|entry| entry.kind == "draft").unwrap();
            assert_eq!(draft.path, snapshot.path);
            let history = entries
                .iter()
                .find(|entry| entry.kind == "history")
                .unwrap();
            assert_eq!(history.path.as_deref(), target.to_str());
            assert_eq!(
                recovery.restore(&history.id).unwrap().content,
                "previous target content"
            );
            let restored = recovery.restore_document(&draft.id, None).unwrap();
            let name = format!("image-{}.png", blake3::hash(b"source image").to_hex());
            assert!(
                restored
                    .document
                    .content
                    .contains(&format!("inkflow-asset://{name}"))
            );
            let resource = crate::asset::pending_asset_path(
                recovery.directory(),
                &restored.document.id,
                &name,
            )
            .unwrap();
            assert_eq!(fs::read(resource).unwrap(), b"source image");
        }
    }

    #[test]
    fn failed_first_save_keeps_the_recovery_draft_untitled() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let store = DocumentStore::new();
        let target = temp.path().join("note.md");
        let result = store.save(
            SaveDocumentRequest {
                id: "untitled".into(),
                path: Some(target.to_string_lossy().into_owned()),
                title: "Untitled".into(),
                content: "![missing](inkflow-asset://missing.png)".into(),
                encoding: "utf-8".into(),
                eol: "lf".into(),
                had_bom: false,
                expected_revision: None,
                history_image_sources: None,
            },
            &recovery,
            Some(target.clone()),
            None,
        );
        assert!(result.is_err());
        assert!(!target.exists());
        let entries = recovery.list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "draft");
        assert!(entries[0].path.is_none());
    }

    #[test]
    fn ordinary_save_rejects_a_stale_path_after_save_as() {
        for source_exists in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("A.md");
            let destination = temp.path().join("B.md");
            fs::write(&source, "same body").unwrap();
            let store = DocumentStore::new();
            let snapshot = store.open_path(&source, None).unwrap();
            let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
            // A reload can finish on the backend before its old response is
            // delivered to the frontend after a successful Save As.
            let old_reload = store.reload(&snapshot.id).unwrap();
            let copied = store
                .save(
                    save_request(&snapshot, &destination, "same body"),
                    &recovery,
                    Some(destination.clone()),
                    None,
                )
                .unwrap();
            assert!(matches!(copied, SaveOutcome::Saved { .. }));
            if source_exists {
                fs::write(&source, "external edit").unwrap();
            } else {
                fs::remove_file(&source).unwrap();
            }
            let rejected = store
                .save(
                    save_request(&old_reload, &source, "stale edit"),
                    &recovery,
                    None,
                    None,
                )
                .unwrap_err();
            assert_eq!(rejected.code, "path_changed");
            assert_eq!(
                store.path_for(&snapshot.id).unwrap(),
                canonical_existing(&destination).unwrap()
            );
            assert_eq!(fs::read_to_string(&destination).unwrap(), "same body");
            if source_exists {
                assert_eq!(fs::read_to_string(&source).unwrap(), "external edit");
            } else {
                assert!(!source.exists());
            }
            // Omitting the path still saves the registered destination.
            let current = store.reload(&snapshot.id).unwrap();
            let mut request = save_request(&current, &destination, "current edit");
            request.path = None;
            assert!(matches!(
                store.save(request, &recovery, None, None).unwrap(),
                SaveOutcome::Saved { .. }
            ));
            assert_eq!(fs::read_to_string(destination).unwrap(), "current edit");
        }
    }

    #[test]
    fn oversized_save_as_image_rolls_back_assets_and_preserves_destination() {
        for existing in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source.md");
            let destination = temp.path().join("Copy.md");
            let content = "![small](small.png)\n![large](large.png)";
            fs::write(&source, content).unwrap();
            fs::write(temp.path().join("small.png"), b"small").unwrap();
            fs::File::create(temp.path().join("large.png"))
                .unwrap()
                .set_len(50 * 1024 * 1024 + 1)
                .unwrap();
            if existing {
                fs::write(&destination, "existing document").unwrap();
            }
            let store = DocumentStore::new();
            let snapshot = store.open_path(&source, None).unwrap();
            let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
            let error = store
                .save(
                    save_request(&snapshot, &destination, content),
                    &recovery,
                    Some(destination.clone()),
                    None,
                )
                .unwrap_err();
            assert_eq!(error.code, "resource_too_large");
            assert!(!temp.path().join("Copy.assets").exists());
            if existing {
                assert_eq!(
                    fs::read_to_string(destination).unwrap(),
                    "existing document"
                );
            } else {
                assert!(!destination.exists());
            }
            assert_eq!(
                store.path_for(&snapshot.id).unwrap(),
                canonical_existing(&source).unwrap()
            );
        }
    }

    #[test]
    fn read_only_source_can_be_saved_as_a_writable_copy() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.md");
        let destination = temp.path().join("Copy.md");
        fs::write(&source, "source").unwrap();
        let original_permissions = fs::metadata(&source).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        fs::set_permissions(&source, permissions).unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&source, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let rejected = store.save(
            save_request(&snapshot, &source, "overwrite"),
            &recovery,
            Some(source.clone()),
            None,
        );
        let copied = store.save(
            save_request(&snapshot, &destination, "source"),
            &recovery,
            Some(destination.clone()),
            None,
        );
        let source_unchanged = fs::read_to_string(&source).unwrap() == "source"
            && fs::metadata(&source).unwrap().permissions().readonly();
        fs::set_permissions(&source, original_permissions).unwrap();
        assert_eq!(rejected.unwrap_err().code, "read_only");
        assert!(matches!(copied.unwrap(), SaveOutcome::Saved { .. }));
        assert!(source_unchanged);
        let copy = store.reload(&snapshot.id).unwrap();
        assert!(!copy.read_only);
        // Follow the snapshot path as the frontend does. Windows temporary
        // paths can use different casing or short names before canonicalization.
        let copy_path = Path::new(copy.path.as_deref().unwrap());
        store
            .save(
                save_request(&copy, copy_path, "edited copy"),
                &recovery,
                None,
                None,
            )
            .unwrap();
        assert_eq!(fs::read_to_string(destination).unwrap(), "edited copy");
    }

    #[test]
    fn slow_external_poll_does_not_block_save_or_publish_a_stale_observation() {
        use std::{
            sync::{Arc, mpsc},
            thread,
        };
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        fs::write(&path, "original").unwrap();
        let store = Arc::new(DocumentStore::new());
        let snapshot = store.open_path(&path, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        fs::write(&path, "external edit").unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let poll_store = Arc::clone(&store);
        let poll = thread::spawn(move || {
            poll_store.check_external_changes_with(|path| {
                let observed = revision(path);
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                observed
            })
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (saved_tx, saved_rx) = mpsc::channel();
        let save_store = Arc::clone(&store);
        let request = save_request(&snapshot, &path, "latest saved content");
        let save_path = path.clone();
        let save = thread::spawn(move || {
            saved_tx
                .send(save_store.save(request, &recovery, Some(save_path), None))
                .unwrap();
        });
        let saved_before_read_finished = saved_rx.recv_timeout(Duration::from_secs(3));
        release_tx.send(()).unwrap();
        let changes = poll.join().unwrap();
        save.join().unwrap();
        assert!(matches!(
            saved_before_read_finished.unwrap().unwrap(),
            SaveOutcome::Saved { .. }
        ));
        assert!(changes.is_empty());
        assert!(store.check_external_changes().is_empty());
    }

    #[test]
    fn batch_poll_discards_earlier_observations_invalidated_during_a_later_read() {
        use std::{
            sync::{Arc, mpsc},
            thread,
        };
        for action in ["save", "reload", "close", "relocate"] {
            let temp = tempfile::tempdir().unwrap();
            let first_path = temp.path().join("a.md");
            let second_path = temp.path().join("b.md");
            fs::write(&first_path, "initial").unwrap();
            fs::write(&second_path, "initial").unwrap();
            let store = Arc::new(DocumentStore::new());
            store
                .open_paths(vec![
                    first_path.to_string_lossy().into_owned(),
                    second_path.to_string_lossy().into_owned(),
                ])
                .unwrap();
            // Follow the store's actual iteration order without relying on HashMap ordering.
            let ordered: Vec<_> = store.documents.read().values().cloned().collect();
            let first = ordered[0].clone();
            let first_id = first.id.clone();
            let second_path = ordered[1].path.clone();
            fs::write(&first.path, "external first").unwrap();
            fs::write(&second_path, "external second").unwrap();
            let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let poll_store = Arc::clone(&store);
            let poll = thread::spawn(move || {
                poll_store.check_external_changes_with(|path| {
                    let observed = revision(path);
                    if path == second_path {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    }
                    observed
                })
            });
            started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let (changed_tx, changed_rx) = mpsc::channel();
            let change_store = Arc::clone(&store);
            let changer = thread::spawn(move || {
                match action {
                    "save" => {
                        let request = SaveDocumentRequest {
                            id: first.id,
                            path: Some(first.path.to_string_lossy().into_owned()),
                            title: "First.md".into(),
                            content: "latest saved body".into(),
                            encoding: "utf-8".into(),
                            eol: "lf".into(),
                            had_bom: false,
                            expected_revision: Some(first.revision),
                            history_image_sources: None,
                        };
                        assert!(matches!(
                            change_store
                                .save(request, &recovery, Some(first.path), None)
                                .unwrap(),
                            SaveOutcome::Saved { .. }
                        ));
                    }
                    "reload" => {
                        change_store.reload(&first.id).unwrap();
                    }
                    "close" => change_store.close(&first.id),
                    "relocate" => {
                        let destination = first.path.with_file_name("moved.md");
                        fs::rename(&first.path, &destination).unwrap();
                        change_store.relocate_paths(&first.path, &destination, false);
                    }
                    _ => unreachable!(),
                }
                changed_tx.send(()).unwrap();
            });
            let changed_while_reading = changed_rx.recv_timeout(Duration::from_secs(3));
            release_tx.send(()).unwrap();
            let changes = poll.join().unwrap();
            changer.join().unwrap();
            assert!(
                changed_while_reading.is_ok(),
                "{action} blocked behind disk I/O"
            );
            assert!(
                changes.iter().all(|change| change.document_id != first_id),
                "stale event after {action}"
            );
            assert_eq!(
                changes.len(),
                1,
                "the other document's current observation must survive"
            );
        }
    }

    #[test]
    fn batch_open_does_not_retain_documents_when_a_later_path_fails() {
        let temp = tempfile::tempdir().unwrap();
        let valid = temp.path().join("valid.md");
        let invalid = temp.path().join("invalid.md");
        fs::write(&valid, "valid").unwrap();
        fs::write(&invalid, [0xff, 0xfe, 0x00]).unwrap();
        let store = DocumentStore::new();

        let result = store.open_paths(vec![
            valid.to_string_lossy().into_owned(),
            invalid.to_string_lossy().into_owned(),
        ]);

        assert!(result.is_err());
        assert!(store.documents.read().is_empty());
    }

    #[test]
    fn unchanged_save_keeps_original_bytes_even_with_mixed_line_endings() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("mixed.md");
        let original = b"\xEF\xBB\xBF# title\r\nfirst\nsecond\r\n";
        fs::write(&path, original).unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&path, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        store
            .save(
                SaveDocumentRequest {
                    id: snapshot.id,
                    path: snapshot.path,
                    title: snapshot.title,
                    content: snapshot.content,
                    encoding: snapshot.encoding,
                    eol: snapshot.eol,
                    had_bom: snapshot.had_bom,
                    expected_revision: snapshot.revision,
                    history_image_sources: None,
                },
                &recovery,
                None,
                None,
            )
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn desktop_save_waits_for_workspace_path_mutations() {
        use std::{sync::Arc, sync::mpsc, thread, time::Duration};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        fs::write(&path, "original").unwrap();
        let store = Arc::new(DocumentStore::new());
        let snapshot = store.open_path(&path, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let first = lock_path_mutations().unwrap();
        let worker_store = Arc::clone(&store);
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = worker_store.save(
                SaveDocumentRequest {
                    id: snapshot.id,
                    path: snapshot.path,
                    title: snapshot.title,
                    content: "edited".into(),
                    encoding: snapshot.encoding,
                    eol: snapshot.eol,
                    had_bom: snapshot.had_bom,
                    expected_revision: snapshot.revision,
                    history_image_sources: None,
                },
                &recovery,
                None,
                None,
            );
            finished_tx.send(result).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "desktop save escaped the workspace path mutation lock"
        );
        drop(first);
        let outcome = finished_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert!(matches!(
            outcome,
            SaveOutcome::Saved {
                recovery_warnings,
                ..
            } if recovery_warnings.is_empty()
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "edited");
        worker.join().unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn ordinary_save_skips_busy_recovery_bookkeeping_without_delaying() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        fs::write(&path, "original").unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&path, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        let recovery_lock =
            crate::data_lock::DataLock::acquire(&recovery.directory().join(".recovery.lock"))
                .unwrap();
        let started = Instant::now();

        let outcome = store
            .save(
                SaveDocumentRequest {
                    id: snapshot.id,
                    path: snapshot.path,
                    title: snapshot.title,
                    content: "edited".into(),
                    encoding: snapshot.encoding,
                    eol: snapshot.eol,
                    had_bom: snapshot.had_bom,
                    expected_revision: snapshot.revision,
                    history_image_sources: None,
                },
                &recovery,
                None,
                None,
            )
            .unwrap();

        assert!(matches!(
            outcome,
            SaveOutcome::Saved {
                recovery_warnings,
                ..
            } if recovery_warnings.is_empty()
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(fs::read_to_string(&path).unwrap(), "edited");
        drop(recovery_lock);
        assert!(recovery.list().unwrap().is_empty());
    }

    #[test]
    fn save_succeeds_when_recovery_checkpoint_storage_fails() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        fs::write(&path, "original").unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&path, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        fs::remove_dir(recovery.directory()).unwrap();
        fs::write(recovery.directory(), "not a directory").unwrap();

        let outcome = store
            .save(
                SaveDocumentRequest {
                    id: snapshot.id,
                    path: snapshot.path,
                    title: snapshot.title,
                    content: "edited".into(),
                    encoding: snapshot.encoding,
                    eol: snapshot.eol,
                    had_bom: snapshot.had_bom,
                    expected_revision: snapshot.revision,
                    history_image_sources: None,
                },
                &recovery,
                None,
                None,
            )
            .unwrap();

        assert!(matches!(
            outcome,
            SaveOutcome::Saved {
                recovery_warnings,
                ..
            } if recovery_warnings.len() == 2
                && recovery_warnings.iter().all(|warning| warning.code == "path_changed")
        ));
        assert_eq!(fs::read_to_string(path).unwrap(), "edited");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn unchanged_save_is_not_delayed_or_failed_by_busy_draft_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        fs::write(&path, "original").unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&path, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        recovery
            .checkpoint(CheckpointRequest {
                document_id: snapshot.id.clone(),
                path: snapshot.path.clone(),
                title: snapshot.title.clone(),
                content: snapshot.content.clone(),
                kind: Some("draft".into()),
            })
            .unwrap();
        let recovery_lock =
            crate::data_lock::DataLock::acquire(&recovery.directory().join(".recovery.lock"))
                .unwrap();
        let started = Instant::now();

        let outcome = store
            .save(
                SaveDocumentRequest {
                    id: snapshot.id,
                    path: snapshot.path,
                    title: snapshot.title,
                    content: snapshot.content,
                    encoding: snapshot.encoding,
                    eol: snapshot.eol,
                    had_bom: snapshot.had_bom,
                    expected_revision: snapshot.revision,
                    history_image_sources: None,
                },
                &recovery,
                None,
                None,
            )
            .unwrap();

        assert!(matches!(outcome, SaveOutcome::Saved { .. }));
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(recovery_lock);
        assert_eq!(
            recovery
                .list()
                .unwrap()
                .iter()
                .filter(|entry| entry.kind == "draft")
                .count(),
            1
        );
    }

    #[test]
    fn relocates_open_documents_with_a_renamed_directory() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir(&source).unwrap();
        let path = source.join("note.md");
        fs::write(&path, "note").unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&path, None).unwrap();
        let canonical_source = canonical_existing(&source).unwrap();

        fs::rename(&source, &destination).unwrap();
        let canonical_destination = canonical_existing(&destination).unwrap();
        store.relocate_paths(&canonical_source, &canonical_destination, true);

        assert_eq!(
            store.path_for(&snapshot.id),
            Some(canonical_destination.join("note.md"))
        );
    }

    #[test]
    fn reports_a_conflict_when_an_open_document_was_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("deleted.md");
        fs::write(&path, "original").unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&path, None).unwrap();
        let recovery = RecoveryStore::new(temp.path().join("recovery")).unwrap();
        fs::remove_file(&path).unwrap();

        let outcome = store
            .save(
                SaveDocumentRequest {
                    id: snapshot.id,
                    path: snapshot.path,
                    title: snapshot.title,
                    content: "local edit".into(),
                    encoding: snapshot.encoding,
                    eol: snapshot.eol,
                    had_bom: snapshot.had_bom,
                    expected_revision: snapshot.revision,
                    history_image_sources: None,
                },
                &recovery,
                None,
                None,
            )
            .unwrap();

        assert!(matches!(
            outcome,
            SaveOutcome::Conflict {
                disk_revision: None,
                ..
            }
        ));
        assert!(!path.exists());
    }

    #[test]
    fn closed_documents_are_removed_from_external_change_tracking() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("closed.md");
        fs::write(&path, "original").unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&path, None).unwrap();
        store.close(&snapshot.id);
        fs::write(&path, "changed").unwrap();

        assert!(store.check_external_changes().is_empty());
        assert!(store.path_for(&snapshot.id).is_none());
    }

    #[test]
    fn completed_reload_cannot_resurrect_a_closed_document() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("closed-during-reload.md");
        fs::write(&path, "original").unwrap();
        let store = DocumentStore::new();
        let snapshot = store.open_path(&path, None).unwrap();
        let baseline = store.documents.read().get(&snapshot.id).unwrap().clone();
        let (_, replacement) = DocumentStore::read_path(&path, snapshot.id.clone()).unwrap();

        store.close(&snapshot.id);
        let result = store.install_reload(&snapshot.id, &baseline, replacement);

        assert!(result.is_err());
        assert!(store.path_for(&snapshot.id).is_none());
    }
}
