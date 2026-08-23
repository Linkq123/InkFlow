use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    data_lock::DataLock,
    error::{ApiError, ApiResult},
    fileio::atomic_write,
    model::{CheckpointRequest, RecoveryEntry, RecoveryRecord, RecoverySnapshot},
};

const MAX_PER_DOCUMENT: usize = 50;
const MAX_AGE_DAYS: i64 = 30;
const MAX_TOTAL_BYTES: u64 = 500 * 1024 * 1024;
const RECOVERY_INDEX_FILE: &str = ".recovery-index-v1.json";

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryIndexV1 {
    schema_version: u32,
    files: Vec<RecoveryFileFingerprint>,
    records: Vec<RecoveryIndexRecord>,
}

impl Default for RecoveryIndexV1 {
    fn default() -> Self {
        Self {
            schema_version: 1,
            files: Vec::new(),
            records: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RecoveryFileFingerprint {
    file_name: String,
    bytes: u64,
    modified_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryIndexRecord {
    file_name: String,
    entry: RecoveryEntry,
    hash: String,
    compressed_bytes: u64,
}

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
        let lock = DataLock::acquire(&self.directory.join(".recovery.lock"))?;
        self.checkpoint_locked(request, lock)
    }

    /// Writes a save-adjacent checkpoint only when the recovery store is
    /// immediately available. Explicit recovery operations use `checkpoint`
    /// and retain their bounded waiting behavior.
    pub fn try_checkpoint(&self, request: CheckpointRequest) -> ApiResult<bool> {
        let Some(lock) = DataLock::try_acquire(&self.directory.join(".recovery.lock"))? else {
            return Ok(false);
        };
        self.checkpoint_locked(request, lock)?;
        Ok(true)
    }

    fn checkpoint_locked(
        &self,
        request: CheckpointRequest,
        _lock: DataLock,
    ) -> ApiResult<Option<RecoveryEntry>> {
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
        let now = Utc::now();
        let minimum_wall_time = chrono::Duration::from_std(minimum)
            .expect("recovery checkpoint intervals fit in chrono::Duration");

        {
            let checkpoints = self.last_checkpoint.lock();
            if let Some((previous_hash, instant)) = checkpoints.get(&key) {
                if in_memory_checkpoint_is_redundant(
                    previous_hash,
                    &hash,
                    instant.elapsed(),
                    minimum,
                ) {
                    return Ok(None);
                }
            }
        }

        let mut index = self.load_or_rebuild_index()?;
        if let Some(protected_id) = checkpoint_redundant_record(
            &index,
            &request.document_id,
            &kind,
            &hash,
            now,
            minimum_wall_time,
        )
        .map(|record| record.entry.id.clone())
        {
            let cleaned = self.cleanup_index_preserving(&mut index, Some(&protected_id));
            if cleaned {
                self.persist_index_if_complete(&index)?;
            }
            return Ok(None);
        }

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
        let file_name = format!("{}-{}.json.zst", now.timestamp_millis(), id);
        atomic_write(&self.directory.join(&file_name), &compressed)?;
        index.files.push(RecoveryFileFingerprint {
            file_name: file_name.clone(),
            bytes: compressed.len() as u64,
            modified_ms: fs::metadata(self.directory.join(&file_name))
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or_default(),
        });
        let checkpoint_file_name = file_name.clone();
        index.records.push(RecoveryIndexRecord {
            file_name,
            entry: entry.clone(),
            hash: hash.clone(),
            compressed_bytes: compressed.len() as u64,
        });
        self.cleanup_index_preserving(&mut index, Some(&entry.id));
        if !index
            .records
            .iter()
            .any(|record| record.entry.id == entry.id)
            || !self.directory.join(&checkpoint_file_name).is_file()
        {
            return Err(ApiError::new(
                "recovery_error",
                "The new recovery snapshot could not be retained.",
            ));
        }
        self.persist_index_if_complete(&index)?;
        self.last_checkpoint
            .lock()
            .insert(key, (hash, Instant::now()));
        Ok(Some(entry))
    }

    pub fn list(&self) -> ApiResult<Vec<RecoveryEntry>> {
        let _lock = DataLock::acquire(&self.directory.join(".recovery.lock"))?;
        let mut records: Vec<_> = self
            .load_or_rebuild_index()?
            .records
            .into_iter()
            .map(|record| record.entry)
            .collect();
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(records)
    }

    pub fn restore(&self, id: &str) -> ApiResult<RecoverySnapshot> {
        let _lock = DataLock::acquire(&self.directory.join(".recovery.lock"))?;
        let index = self.load_or_rebuild_index()?;
        if let Some(indexed) = index.records.iter().find(|record| record.entry.id == id) {
            let record = self.read_record(&self.directory.join(&indexed.file_name))?;
            return Ok(RecoverySnapshot {
                entry: record.entry,
                content: record.content,
            });
        }
        Err(ApiError::new(
            "recovery_not_found",
            "The requested recovery snapshot no longer exists.",
        ))
    }

    pub fn delete(&self, id: &str) -> ApiResult<bool> {
        let _lock = DataLock::acquire(&self.directory.join(".recovery.lock"))?;
        let mut index = self.load_or_rebuild_index()?;
        let Some(record) = index
            .records
            .iter()
            .find(|record| record.entry.id == id)
            .cloned()
        else {
            return Ok(false);
        };
        fs::remove_file(self.directory.join(&record.file_name))
            .map_err(|error| ApiError::io("Unable to remove the recovery snapshot", error))?;
        index.records.retain(|candidate| candidate.entry.id != id);
        index
            .files
            .retain(|candidate| candidate.file_name != record.file_name);
        self.persist_index_if_complete(&index)?;
        Ok(true)
    }

    #[cfg(test)]
    pub fn delete_document_kind(&self, document_id: &str, kind: &str) -> ApiResult<()> {
        let lock = DataLock::acquire(&self.directory.join(".recovery.lock"))?;
        self.delete_document_kind_locked(document_id, kind, lock)
    }

    pub fn try_delete_document_kind(&self, document_id: &str, kind: &str) -> ApiResult<bool> {
        let Some(lock) = DataLock::try_acquire(&self.directory.join(".recovery.lock"))? else {
            return Ok(false);
        };
        self.delete_document_kind_locked(document_id, kind, lock)?;
        Ok(true)
    }

    fn delete_document_kind_locked(
        &self,
        document_id: &str,
        kind: &str,
        _lock: DataLock,
    ) -> ApiResult<()> {
        let mut index = self.load_or_rebuild_index()?;
        let targets = index
            .records
            .iter()
            .filter(|record| record.entry.document_id == document_id && record.entry.kind == kind)
            .map(|record| record.file_name.clone())
            .collect::<HashSet<_>>();
        for file_name in &targets {
            fs::remove_file(self.directory.join(file_name)).map_err(|error| {
                ApiError::io("Unable to remove a completed recovery snapshot", error)
            })?;
        }
        index
            .records
            .retain(|record| !targets.contains(&record.file_name));
        index
            .files
            .retain(|file| !targets.contains(&file.file_name));
        self.persist_index_if_complete(&index)?;
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

    fn load_or_rebuild_index(&self) -> ApiResult<RecoveryIndexV1> {
        let paths = self.record_files()?;
        let fingerprints = recovery_fingerprints(&paths)?;
        if let Ok(bytes) = fs::read(self.directory.join(RECOVERY_INDEX_FILE))
            && let Ok(index) = serde_json::from_slice::<RecoveryIndexV1>(&bytes)
            && recovery_index_matches(&index, &fingerprints)
        {
            return Ok(index);
        }

        let mut index = RecoveryIndexV1 {
            files: fingerprints,
            ..RecoveryIndexV1::default()
        };
        for path in paths {
            let Ok(record) = self.read_record(&path) else {
                continue;
            };
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let compressed_bytes = fs::metadata(&path).map(|value| value.len()).unwrap_or(0);
            index.records.push(RecoveryIndexRecord {
                file_name: file_name.to_string(),
                entry: record.entry,
                hash: record.hash,
                compressed_bytes,
            });
        }
        // Reads must remain available even if a damaged or read-only data
        // directory prevents this optional acceleration index from persisting.
        let _ = self.persist_index_if_complete(&index);
        Ok(index)
    }

    fn persist_index_if_complete(&self, index: &RecoveryIndexV1) -> ApiResult<()> {
        if recovery_index_is_complete(index) {
            self.persist_index(index)?;
        }
        Ok(())
    }

    fn persist_index(&self, index: &RecoveryIndexV1) -> ApiResult<()> {
        let bytes = serde_json::to_vec(index)
            .map_err(|error| ApiError::new("recovery_error", error.to_string()))?;
        atomic_write(&self.directory.join(RECOVERY_INDEX_FILE), &bytes)
    }

    #[cfg(test)]
    fn cleanup_index(&self, index: &mut RecoveryIndexV1) {
        self.cleanup_index_preserving(index, None);
    }

    fn cleanup_index_preserving(
        &self,
        index: &mut RecoveryIndexV1,
        protected_id: Option<&str>,
    ) -> bool {
        let records_before = index.records.len();
        let files_before = index.files.len();
        let valid_names = index
            .records
            .iter()
            .map(|record| record.file_name.clone())
            .collect::<HashSet<_>>();
        let mut retained_unreadable = HashSet::new();
        let mut total = 0u64;
        for file in &index.files {
            if valid_names.contains(&file.file_name) {
                continue;
            }
            // A missing index record can mean the snapshot was temporarily
            // unreadable. Keep it for a later rebuild instead of treating an
            // acceleration-index miss as authority to delete recovery data.
            retained_unreadable.insert(file.file_name.clone());
            total = total.saturating_add(file.bytes);
        }

        let cutoff = Utc::now() - chrono::Duration::days(MAX_AGE_DAYS);
        index.records.sort_by(|left, right| {
            let left_protected = protected_id == Some(left.entry.id.as_str());
            let right_protected = protected_id == Some(right.entry.id.as_str());
            right_protected
                .cmp(&left_protected)
                .then_with(|| recovery_created_at(right).cmp(&recovery_created_at(left)))
        });
        let mut per_document = HashMap::<String, usize>::new();
        let mut retained_records = Vec::with_capacity(index.records.len());
        for record in std::mem::take(&mut index.records) {
            let count = per_document
                .entry(record.entry.document_id.clone())
                .or_default();
            let protected = protected_id == Some(record.entry.id.as_str());
            let should_prune = !protected
                && (recovery_created_at(&record) < cutoff
                    || *count >= MAX_PER_DOCUMENT
                    || total.saturating_add(record.compressed_bytes) > MAX_TOTAL_BYTES);
            if should_prune && fs::remove_file(self.directory.join(&record.file_name)).is_ok() {
                continue;
            }
            *count += 1;
            total = total.saturating_add(record.compressed_bytes);
            retained_records.push(record);
        }
        let retained_names = retained_records
            .iter()
            .map(|record| record.file_name.clone())
            .collect::<HashSet<_>>();
        index.files.retain(|file| {
            retained_names.contains(&file.file_name)
                || retained_unreadable.contains(&file.file_name)
        });
        index
            .files
            .sort_by(|left, right| left.file_name.cmp(&right.file_name));
        index.records = retained_records;
        index.records.len() != records_before || index.files.len() != files_before
    }
}

fn checkpoint_redundant_record<'a>(
    index: &'a RecoveryIndexV1,
    document_id: &str,
    kind: &str,
    hash: &str,
    now: DateTime<Utc>,
    minimum_wall_time: chrono::Duration,
) -> Option<&'a RecoveryIndexRecord> {
    let retention_cutoff = now - chrono::Duration::days(MAX_AGE_DAYS);
    index.records.iter().find(|record| {
        if record.entry.document_id != document_id || record.entry.kind != kind {
            return false;
        }
        let within_minimum = DateTime::parse_from_rfc3339(&record.entry.created_at)
            .map(|created| is_within_checkpoint_interval(now, created, minimum_wall_time))
            .unwrap_or(false);
        let retained_matching_hash =
            record.hash == hash && recovery_created_at(record) >= retention_cutoff;
        retained_matching_hash || within_minimum
    })
}

fn in_memory_checkpoint_is_redundant(
    previous_hash: &str,
    hash: &str,
    elapsed: Duration,
    minimum: Duration,
) -> bool {
    let retention = Duration::from_secs(MAX_AGE_DAYS as u64 * 24 * 60 * 60);
    elapsed < minimum || (previous_hash == hash && elapsed < retention)
}

fn is_within_checkpoint_interval(
    now: DateTime<Utc>,
    created: DateTime<chrono::FixedOffset>,
    minimum: chrono::Duration,
) -> bool {
    let elapsed = now.signed_duration_since(created);
    elapsed >= chrono::Duration::zero() && elapsed < minimum
}

fn recovery_index_is_complete(index: &RecoveryIndexV1) -> bool {
    if index.schema_version != 1 || index.files.len() != index.records.len() {
        return false;
    }
    let file_names = index
        .files
        .iter()
        .map(|file| file.file_name.as_str())
        .collect::<HashSet<_>>();
    let record_names = index
        .records
        .iter()
        .map(|record| record.file_name.as_str())
        .collect::<HashSet<_>>();
    file_names.len() == index.files.len()
        && record_names.len() == index.records.len()
        && file_names == record_names
}

fn recovery_index_matches(
    index: &RecoveryIndexV1,
    fingerprints: &[RecoveryFileFingerprint],
) -> bool {
    recovery_index_is_complete(index) && index.files == fingerprints
}

fn recovery_fingerprints(paths: &[PathBuf]) -> ApiResult<Vec<RecoveryFileFingerprint>> {
    let mut fingerprints = Vec::with_capacity(paths.len());
    for path in paths {
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| ApiError::new("recovery_error", "A recovery filename is invalid."))?;
        let metadata = fs::metadata(path)
            .map_err(|error| ApiError::io("Unable to inspect a recovery snapshot", error))?;
        let bytes = metadata.len();
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or_default();
        fingerprints.push(RecoveryFileFingerprint {
            file_name: file_name.to_string(),
            bytes,
            modified_ms,
        });
    }
    fingerprints.sort_by(|left, right| left.file_name.cmp(&right.file_name));
    Ok(fingerprints)
}

fn recovery_created_at(record: &RecoveryIndexRecord) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&record.entry.created_at)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

#[cfg(test)]
fn prune_records(
    records: Vec<(PathBuf, String, DateTime<Utc>, u64)>,
    max_per_document: usize,
    max_total_bytes: u64,
) {
    prune_records_with(records, max_per_document, max_total_bytes, |path| {
        fs::remove_file(path)
    });
}

#[cfg(test)]
fn prune_records_with<F>(
    records: Vec<(PathBuf, String, DateTime<Utc>, u64)>,
    max_per_document: usize,
    max_total_bytes: u64,
    mut remove: F,
) where
    F: FnMut(&Path) -> std::io::Result<()>,
{
    let mut per_document = HashMap::<String, usize>::new();
    let mut total = 0u64;
    for (path, document, _, bytes) in records {
        let count = per_document.entry(document).or_default();
        let should_prune =
            *count >= max_per_document || total.saturating_add(bytes) > max_total_bytes;
        if should_prune && remove(&path).is_ok() {
            continue;
        }
        *count += 1;
        total = total.saturating_add(bytes);
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

    #[test]
    fn future_checkpoint_timestamp_does_not_throttle_after_clock_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let request = |content: &str| CheckpointRequest {
            document_id: "clock-rollback".into(),
            path: Some("C:\\notes\\rollback.md".into()),
            title: "Rollback".into(),
            content: content.into(),
            kind: Some("history".into()),
        };
        assert!(store.checkpoint(request("first")).unwrap().is_some());

        let index_path = temp.path().join(RECOVERY_INDEX_FILE);
        let mut index =
            serde_json::from_slice::<RecoveryIndexV1>(&fs::read(&index_path).unwrap()).unwrap();
        index.records[0].entry.created_at =
            (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        fs::write(&index_path, serde_json::to_vec(&index).unwrap()).unwrap();

        let reopened = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        assert!(reopened.checkpoint(request("second")).unwrap().is_some());
        assert_eq!(reopened.list().unwrap().len(), 2);
    }

    #[test]
    fn new_checkpoint_survives_cleanup_when_older_entries_have_future_timestamps() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        for index in 0..MAX_PER_DOCUMENT {
            store
                .checkpoint(CheckpointRequest {
                    document_id: "clock-rollback-limit".into(),
                    path: Some("C:\\notes\\rollback-limit.md".into()),
                    title: "Rollback limit".into(),
                    content: format!("future content {index}"),
                    kind: Some(format!("future-kind-{index}")),
                })
                .unwrap()
                .unwrap();
        }

        let index_path = temp.path().join(RECOVERY_INDEX_FILE);
        let mut recovery_index =
            serde_json::from_slice::<RecoveryIndexV1>(&fs::read(&index_path).unwrap()).unwrap();
        for (offset, record) in recovery_index.records.iter_mut().enumerate() {
            record.entry.created_at =
                (Utc::now() + chrono::Duration::days(1) + chrono::Duration::seconds(offset as i64))
                    .to_rfc3339();
        }
        store.persist_index(&recovery_index).unwrap();

        let created = store
            .checkpoint(CheckpointRequest {
                document_id: "clock-rollback-limit".into(),
                path: Some("C:\\notes\\rollback-limit.md".into()),
                title: "Rollback limit".into(),
                content: "current content".into(),
                kind: Some("current".into()),
            })
            .unwrap()
            .unwrap();

        assert_eq!(
            store.restore(&created.id).unwrap().content,
            "current content"
        );
        assert_eq!(store.list().unwrap().len(), MAX_PER_DOCUMENT);
    }

    #[test]
    fn corrupt_records_do_not_block_valid_recovery_entries() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let entry = store
            .checkpoint(CheckpointRequest {
                document_id: "doc".into(),
                path: None,
                title: "Draft".into(),
                content: "recover me".into(),
                kind: None,
            })
            .unwrap()
            .unwrap();
        fs::write(temp.path().join("0000-corrupt.json.zst"), b"not zstd").unwrap();

        assert_eq!(store.restore(&entry.id).unwrap().content, "recover me");
        assert!(store.delete(&entry.id).unwrap());
        assert!(!store.delete(&entry.id).unwrap());
        assert!(store.restore(&entry.id).is_err());
    }

    #[test]
    fn separate_process_stores_share_checkpoint_throttling() {
        let temp = tempfile::tempdir().unwrap();
        let first = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let second = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let request = |content: &str| CheckpointRequest {
            document_id: "shared-document".into(),
            path: Some("C:\\notes\\shared.md".into()),
            title: "Shared".into(),
            content: content.into(),
            kind: Some("history".into()),
        };

        assert!(first.checkpoint(request("first")).unwrap().is_some());
        assert!(second.checkpoint(request("second")).unwrap().is_none());
        assert_eq!(second.list().unwrap().len(), 1);
    }

    #[test]
    fn an_expired_matching_hash_is_replaced_instead_of_bypassing_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let request = || CheckpointRequest {
            document_id: "expired-deduplication".into(),
            path: Some("C:\\notes\\expired.md".into()),
            title: "Expired".into(),
            content: "unchanged content".into(),
            kind: Some("history".into()),
        };
        let expired = store.checkpoint(request()).unwrap().unwrap();
        let mut index = store.load_or_rebuild_index().unwrap();
        index.records[0].entry.created_at =
            (Utc::now() - chrono::Duration::days(MAX_AGE_DAYS + 1)).to_rfc3339();
        store.persist_index(&index).unwrap();

        let reopened = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let refreshed = reopened.checkpoint(request()).unwrap().unwrap();

        assert_ne!(refreshed.id, expired.id);
        assert!(reopened.restore(&expired.id).is_err());
        let listed = reopened.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, refreshed.id);
    }

    #[test]
    fn in_memory_hash_deduplication_expires_with_the_recovery_retention_window() {
        let minimum = Duration::from_secs(60);
        assert!(in_memory_checkpoint_is_redundant(
            "same",
            "same",
            Duration::from_secs(29 * 24 * 60 * 60),
            minimum,
        ));
        assert!(!in_memory_checkpoint_is_redundant(
            "same",
            "same",
            Duration::from_secs(31 * 24 * 60 * 60),
            minimum,
        ));
        assert!(in_memory_checkpoint_is_redundant(
            "old",
            "new",
            Duration::from_secs(30),
            minimum,
        ));
    }

    #[test]
    fn recovery_index_is_persisted_and_rebuilt_when_damaged() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let entry = store
            .checkpoint(CheckpointRequest {
                document_id: "indexed-document".into(),
                path: Some("C:\\notes\\indexed.md".into()),
                title: "Indexed".into(),
                content: "indexed content".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let index_path = temp.path().join(RECOVERY_INDEX_FILE);
        assert!(index_path.is_file());
        fs::write(&index_path, b"damaged-index").unwrap();

        let reopened = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let listed = reopened.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, entry.id);
        assert_eq!(
            reopened.restore(&entry.id).unwrap().content,
            "indexed content"
        );
        assert!(serde_json::from_slice::<RecoveryIndexV1>(&fs::read(index_path).unwrap()).is_ok());
    }

    #[test]
    fn incomplete_recovery_index_is_rebuilt_instead_of_being_trusted() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let entry = store
            .checkpoint(CheckpointRequest {
                document_id: "incomplete-index-document".into(),
                path: None,
                title: "Incomplete index".into(),
                content: "recoverable content".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let index_path = temp.path().join(RECOVERY_INDEX_FILE);
        let mut incomplete =
            serde_json::from_slice::<RecoveryIndexV1>(&fs::read(&index_path).unwrap()).unwrap();
        incomplete.records.clear();
        fs::write(&index_path, serde_json::to_vec(&incomplete).unwrap()).unwrap();

        let reopened = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let listed = reopened.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, entry.id);

        let rebuilt =
            serde_json::from_slice::<RecoveryIndexV1>(&fs::read(index_path).unwrap()).unwrap();
        assert!(recovery_index_is_complete(&rebuilt));
        assert_eq!(rebuilt.records.len(), 1);
    }

    #[test]
    fn cleanup_keeps_snapshots_missing_from_the_acceleration_index() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        store
            .checkpoint(CheckpointRequest {
                document_id: "unindexed-document".into(),
                path: None,
                title: "Unindexed".into(),
                content: "keep me".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let mut index = serde_json::from_slice::<RecoveryIndexV1>(
            &fs::read(temp.path().join(RECOVERY_INDEX_FILE)).unwrap(),
        )
        .unwrap();
        let snapshot = temp.path().join(&index.files[0].file_name);
        index.records.clear();

        store.cleanup_index(&mut index);

        assert!(snapshot.is_file());
        assert_eq!(index.files.len(), 1);
        assert!(!recovery_index_is_complete(&index));
    }

    #[test]
    fn pruned_document_records_do_not_consume_the_global_budget() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let pruned = temp.path().join("pruned");
        let other = temp.path().join("other");
        for path in [&first, &pruned, &other] {
            fs::write(path, b"record").unwrap();
        }
        let now = Utc::now();
        prune_records(
            vec![
                (first.clone(), "document-a".into(), now, 40),
                (pruned.clone(), "document-a".into(), now, 40),
                (other.clone(), "document-b".into(), now, 50),
            ],
            1,
            100,
        );

        assert!(first.exists());
        assert!(!pruned.exists());
        assert!(other.exists());
    }

    #[test]
    fn records_that_cannot_be_pruned_still_consume_the_global_budget() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let locked = temp.path().join("locked");
        let other = temp.path().join("other");
        for path in [&first, &locked, &other] {
            fs::write(path, b"record").unwrap();
        }
        let now = Utc::now();
        prune_records_with(
            vec![
                (first.clone(), "document-a".into(), now, 40),
                (locked.clone(), "document-a".into(), now, 40),
                (other.clone(), "document-b".into(), now, 50),
            ],
            1,
            100,
            |path| {
                if path == locked {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "locked",
                    ))
                } else {
                    fs::remove_file(path)
                }
            },
        );

        assert!(first.exists());
        assert!(locked.exists());
        assert!(!other.exists());
    }
}
