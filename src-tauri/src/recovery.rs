use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    fileio::atomic_write,
    model::{CheckpointRequest, RecoveryEntry, RecoveryRecord, RecoverySnapshot},
};

const MAX_PER_DOCUMENT: usize = 50;
const MAX_AGE_DAYS: i64 = 30;
const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;

pub struct RecoveryStore {
    directory: PathBuf,
    last_checkpoint: Mutex<HashMap<(String, String), (String, Instant)>>,
}

impl RecoveryStore {
    pub fn new(directory: PathBuf) -> ApiResult<Self> {
        fs::create_dir_all(&directory)
            .map_err(|error| ApiError::io("Unable to create the recovery directory", error))?;
        Ok(Self {
            directory,
            last_checkpoint: Mutex::new(HashMap::new()),
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn checkpoint(&self, request: CheckpointRequest) -> ApiResult<Option<RecoveryEntry>> {
        let kind = request.kind.unwrap_or_else(|| "draft".into());
        let hash = blake3::hash(request.content.as_bytes())
            .to_hex()
            .to_string();
        let key = (request.document_id.clone(), kind.clone());
        let minimum = if kind == "history" {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(2)
        };

        {
            let checkpoints = self.last_checkpoint.lock();
            if let Some((previous_hash, instant)) = checkpoints.get(&key) {
                if previous_hash == &hash || instant.elapsed() < minimum {
                    return Ok(None);
                }
            }
        }

        let now = Utc::now();
        let id = Uuid::new_v4().to_string();
        let entry = RecoveryEntry {
            id: id.clone(),
            document_id: request.document_id,
            path: request.path,
            title: request.title,
            created_at: now.to_rfc3339(),
            kind,
            size: request.content.len(),
        };
        let record = RecoveryRecord {
            entry: entry.clone(),
            content: request.content,
            hash: hash.clone(),
        };
        let json = serde_json::to_vec(&record)
            .map_err(|error| ApiError::new("recovery_error", error.to_string()))?;
        let compressed = zstd::stream::encode_all(Cursor::new(json), 3)
            .map_err(|error| ApiError::io("Unable to compress the recovery snapshot", error))?;
        let name = format!("{}-{}.json.zst", now.timestamp_millis(), id);
        atomic_write(&self.directory.join(name), &compressed)?;
        self.last_checkpoint
            .lock()
            .insert(key, (hash, Instant::now()));
        self.cleanup()?;
        Ok(Some(entry))
    }

    pub fn list(&self) -> ApiResult<Vec<RecoveryEntry>> {
        let mut records: Vec<_> = self
            .record_files()?
            .into_iter()
            .filter_map(|path| self.read_record(&path).ok().map(|record| record.entry))
            .collect();
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(records)
    }

    pub fn restore(&self, id: &str) -> ApiResult<RecoverySnapshot> {
        for path in self.record_files()? {
            let record = self.read_record(&path)?;
            if record.entry.id == id {
                return Ok(RecoverySnapshot {
                    entry: record.entry,
                    content: record.content,
                });
            }
        }
        Err(ApiError::new(
            "recovery_not_found",
            "The requested recovery snapshot no longer exists.",
        ))
    }

    pub fn delete(&self, id: &str) -> ApiResult<()> {
        for path in self.record_files()? {
            if self.read_record(&path)?.entry.id == id {
                fs::remove_file(path).map_err(|error| {
                    ApiError::io("Unable to remove the recovery snapshot", error)
                })?;
                return Ok(());
            }
        }
        Ok(())
    }

    pub fn delete_document_kind(&self, document_id: &str, kind: &str) -> ApiResult<()> {
        for path in self.record_files()? {
            let record = self.read_record(&path)?;
            if record.entry.document_id == document_id && record.entry.kind == kind {
                fs::remove_file(path).map_err(|error| {
                    ApiError::io("Unable to remove a completed recovery snapshot", error)
                })?;
            }
        }
        self.last_checkpoint
            .lock()
            .remove(&(document_id.to_string(), kind.to_string()));
        Ok(())
    }

    fn read_record(&self, path: &Path) -> ApiResult<RecoveryRecord> {
        let compressed = fs::read(path)
            .map_err(|error| ApiError::io("Unable to read a recovery snapshot", error))?;
        let json = zstd::stream::decode_all(Cursor::new(compressed))
            .map_err(|error| ApiError::io("Unable to decompress a recovery snapshot", error))?;
        serde_json::from_slice(&json)
            .map_err(|error| ApiError::new("recovery_error", error.to_string()))
    }

    fn record_files(&self) -> ApiResult<Vec<PathBuf>> {
        let mut files = Vec::new();
        for item in fs::read_dir(&self.directory)
            .map_err(|error| ApiError::io("Unable to scan the recovery directory", error))?
        {
            let path = item
                .map_err(|error| ApiError::io("Unable to inspect a recovery entry", error))?
                .path();
            if path.extension().and_then(|value| value.to_str()) == Some("zst") {
                files.push(path);
            }
        }
        Ok(files)
    }

    fn cleanup(&self) -> ApiResult<()> {
        let cutoff = Utc::now() - chrono::Duration::days(MAX_AGE_DAYS);
        let mut records = Vec::new();
        for path in self.record_files()? {
            match self.read_record(&path) {
                Ok(record) => {
                    let created = DateTime::parse_from_rfc3339(&record.entry.created_at)
                        .map(|value| value.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now());
                    let bytes = fs::metadata(&path).map(|value| value.len()).unwrap_or(0);
                    if created < cutoff {
                        let _ = fs::remove_file(path);
                    } else {
                        records.push((path, record.entry.document_id, created, bytes));
                    }
                }
                Err(_) => {
                    let _ = fs::remove_file(path);
                }
            }
        }

        records.sort_by(|left, right| right.2.cmp(&left.2));
        let mut per_document = HashMap::<String, usize>::new();
        let mut total = 0u64;
        for (path, document, _, bytes) in records {
            let count = per_document.entry(document).or_default();
            *count += 1;
            total += bytes;
            if *count > MAX_PER_DOCUMENT || total > MAX_TOTAL_BYTES {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_restores_a_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let entry = store
            .checkpoint(CheckpointRequest {
                document_id: "doc".into(),
                path: None,
                title: "Untitled".into(),
                content: "中文 content".into(),
                kind: None,
            })
            .unwrap()
            .unwrap();
        assert_eq!(store.restore(&entry.id).unwrap().content, "中文 content");
        store.delete_document_kind("doc", "draft").unwrap();
        assert!(store.list().unwrap().is_empty());
    }
}
