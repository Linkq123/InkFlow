use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use parking_lot::{Mutex, RwLock};
use uuid::Uuid;

use crate::{
    asset::{cleanup_pending_assets, copy_referenced_assets_for_save_as, migrate_pending_assets},
    encoding,
    error::{ApiError, ApiResult},
    fileio::{atomic_write, canonical_existing, revision, revision_from_bytes},
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
    content_hash: String,
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
        let id = existing_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let title = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Untitled.md")
            .to_string();

        self.documents.write().insert(
            id.clone(),
            DocumentMeta {
                id: id.clone(),
                path: path.clone(),
                revision: revision.clone(),
                content_hash: blake3::hash(decoded.content.as_bytes())
                    .to_hex()
                    .to_string(),
            },
        );

        Ok(DocumentSnapshot {
            id,
            path: Some(path.to_string_lossy().into_owned()),
            title,
            content: decoded.content,
            encoding: decoded.encoding,
            eol: decoded.eol,
            had_bom: decoded.had_bom,
            had_final_newline: decoded.had_final_newline,
            read_only: metadata.permissions().readonly(),
            revision: Some(revision),
        })
    }

    pub fn reload(&self, document_id: &str) -> ApiResult<DocumentSnapshot> {
        let meta = self
            .documents
            .read()
            .get(document_id)
            .cloned()
            .ok_or_else(|| ApiError::new("document_not_found", "The document is not open."))?;
        self.open_path(&meta.path, Some(document_id.to_string()))
    }

    pub fn save(
        &self,
        mut request: SaveDocumentRequest,
        recovery: &RecoveryStore,
        force_path: Option<PathBuf>,
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
        if path.exists() && !conflict_was_confirmed {
            let disk = revision(&path)?;
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
                request.content =
                    copy_referenced_assets_for_save_as(source, &path, &request.content)?;
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
        atomic_write(&path, &bytes)?;
        let disk_revision = revision_from_bytes(&path, &bytes)?;
        let canonical = canonical_existing(&path)?;
        self.documents.write().insert(
            request.id.clone(),
            DocumentMeta {
                id: request.id.clone(),
                path: canonical.clone(),
                revision: disk_revision.clone(),
                content_hash: blake3::hash(request.content.as_bytes())
                    .to_hex()
                    .to_string(),
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
            .read()
            .values()
            .filter_map(|meta| {
                if !meta.path.exists() {
                    return Some(ExternalChange {
                        document_id: meta.id.clone(),
                        path: meta.path.to_string_lossy().into_owned(),
                        kind: "deleted".into(),
                        revision: None,
                    });
                }
                match revision(&meta.path) {
                    Ok(current) if current != meta.revision => Some(ExternalChange {
                        document_id: meta.id.clone(),
                        path: meta.path.to_string_lossy().into_owned(),
                        kind: "modified".into(),
                        revision: Some(current),
                    }),
                    _ => None,
                }
            })
            .collect()
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
}
