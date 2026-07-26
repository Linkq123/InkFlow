use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use uuid::Uuid;

use crate::{
    asset::{cleanup_pending_assets, copy_referenced_assets_for_save_as, migrate_pending_assets},
    encoding,
    error::{ApiError, ApiResult},
    fileio::{
        AtomicWriteOutcome, atomic_create_if_absent, atomic_write_if_revision, canonical_existing,
        revision, revision_from_bytes, revision_metadata,
    },
    model::{
        CheckpointRequest, DiskRevision, DocumentSnapshot, ExternalChange, SaveDocumentRequest,
        SaveOutcome,
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
        paths
            .into_iter()
            .map(|path| self.open_path(Path::new(&path), None))
            .collect()
    }

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
        let known = self.documents.read().get(&request.id).cloned();
        let explicit_save_as = force_path.is_some();
        let path = force_path
            .or_else(|| request.path.as_ref().map(PathBuf::from))
            .or_else(|| known.as_ref().map(|value| value.path.clone()));
        let Some(path) = path else {
            return Ok(SaveOutcome::NeedsPath);
        };

        let path_changed = known.as_ref().is_none_or(|value| value.path != path);
        let conflict_was_confirmed = explicit_save_as || path_changed;
        if !path.exists() && !conflict_was_confirmed && known.is_some() {
            return Ok(SaveOutcome::Conflict {
                path: path.to_string_lossy().into_owned(),
                disk_revision: None,
            });
        }
        let mut validated_revision = None;
        if path.exists() {
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
                {
                    let _ = recovery.delete_document_kind(&request.id, "draft");
                    return Ok(SaveOutcome::Saved {
                        path: path.to_string_lossy().into_owned(),
                        revision: disk,
                        content: None,
                    });
                }
            }
            validated_revision = Some(disk);
        }

        let _ = recovery.checkpoint(CheckpointRequest {
            document_id: request.id.clone(),
            path: request.path.clone(),
            title: request.title.clone(),
            content: request.content.clone(),
            kind: Some("draft".into()),
        });

        if path.exists() {
            if let Ok(previous_bytes) = fs::read(&path) {
                if let Ok(previous) = encoding::decode(&previous_bytes) {
                    let _ = recovery.checkpoint(CheckpointRequest {
                        document_id: request.id.clone(),
                        path: Some(path.to_string_lossy().into_owned()),
                        title: request.title.clone(),
                        content: previous.content,
                        kind: Some("history".into()),
                    });
                }
            }
        }

        let original_content = request.content.clone();
        if path_changed {
            if let Some(source) = known.as_ref().map(|value| value.path.as_path()) {
                request.content = copy_referenced_assets_for_save_as(
                    source,
                    &path,
                    &request.content,
                    workspace_root,
                )?;
            }
        }
        request.content =
            migrate_pending_assets(recovery.directory(), &request.id, &path, &request.content)?;
        let changed_content =
            (request.content != original_content).then(|| request.content.clone());
        let bytes = encoding::encode(
            &request.content,
            &request.encoding,
            &request.eol,
            request.had_bom,
        )?;
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
        let _ = cleanup_pending_assets(recovery.directory(), &request.id);
        let _ = recovery.delete_document_kind(&request.id, "draft");
        Ok(SaveOutcome::Saved {
            path: canonical.to_string_lossy().into_owned(),
            revision: disk_revision,
            content: changed_content,
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
        self.documents
            .write()
            .values_mut()
            .filter_map(|meta| {
                if !meta.path.exists() {
                    return Some(ExternalChange {
                        document_id: meta.id.clone(),
                        path: meta.path.to_string_lossy().into_owned(),
                        kind: "deleted".into(),
                        revision: None,
                    });
                }
                let Ok((modified_ms, size)) = revision_metadata(&meta.path) else {
                    return None;
                };
                let metadata_changed = modified_ms != meta.observed_revision.modified_ms
                    || size != meta.observed_revision.size;
                let hash_due = meta.last_hash_check.elapsed() >= Duration::from_secs(60);
                if metadata_changed || hash_due {
                    let Ok(current) = revision(&meta.path) else {
                        return None;
                    };
                    meta.observed_revision = current;
                    meta.last_hash_check = Instant::now();
                }
                (meta.observed_revision != meta.revision).then(|| ExternalChange {
                    document_id: meta.id.clone(),
                    path: meta.path.to_string_lossy().into_owned(),
                    kind: "modified".into(),
                    revision: Some(meta.observed_revision.clone()),
                })
            })
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

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
                },
                &recovery,
                None,
                None,
            )
            .unwrap();
        assert_eq!(fs::read(path).unwrap(), original);
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
