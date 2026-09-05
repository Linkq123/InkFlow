use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    data_lock::DataLock,
    error::{ApiError, ApiResult},
    fileio::{
        DirectoryIdentityGuard, FileIdentity, atomic_write, canonical_existing, directory_identity,
        guard_directory_identity, is_symbolic_link_or_junction,
    },
    model::{CheckpointRequest, RecoveryEntry, RecoveryRecord, RecoverySnapshot},
};

const MAX_PER_DOCUMENT: usize = 50;
const MAX_AGE_DAYS: i64 = 30;
pub const MAX_RECOVERY_CONTENT_BYTES: usize = 32 * 1024 * 1024;
const MAX_RECOVERY_RECORD_BYTES: usize = 33 * 1024 * 1024;
const MAX_COMPRESSED_RECORD_BYTES: u64 = 33 * 1024 * 1024;
// InkFlow's encoder never needs a window larger than the 32 MiB content cap.
// Setting this explicitly prevents a crafted frame header from reserving the
// zstd streaming decoder's much larger default window.
const MAX_RECOVERY_WINDOW_LOG: u32 = 25;
const MAX_TOTAL_COMPRESSED_BYTES: u64 = 500 * 1024 * 1024;
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 500 * 1024 * 1024;
const RECOVERY_INDEX_FILE: &str = ".recovery-index-v2.json";
const LEGACY_RECOVERY_INDEX_FILE: &str = ".recovery-index-v1.json";
const QUARANTINE_DIRECTORY: &str = "Quarantine";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryIndexV2 {
    schema_version: u32,
    files: Vec<RecoveryFileFingerprint>,
    records: Vec<RecoveryIndexRecord>,
    #[serde(default)]
    integrity: String,
}

impl Default for RecoveryIndexV2 {
    fn default() -> Self {
        Self {
            schema_version: 2,
            files: Vec::new(),
            records: Vec::new(),
            integrity: String::new(),
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
    uncompressed_bytes: u64,
}

struct CountingWriter<W> {
    inner: W,
    bytes: u64,
    limit: u64,
    limit_exceeded: bool,
}

impl<W> CountingWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            bytes: 0,
            limit,
            limit_exceeded: false,
        }
    }

    fn into_parts(self) -> (W, u64) {
        (self.inner, self.bytes)
    }

    fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.bytes.saturating_add(buffer.len() as u64) > self.limit {
            self.limit_exceeded = true;
            return Err(std::io::Error::other(
                "the recovery snapshot exceeded its JSON size limit",
            ));
        }
        let written = self.inner.write(buffer)?;
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub struct RecoveryStore {
    directory: PathBuf,
    directory_identity: FileIdentity,
    last_checkpoint: Mutex<HashMap<(String, String), (String, Instant)>>,
}

struct GuardedQuarantineDirectory {
    path: PathBuf,
    _recovery_guard: DirectoryIdentityGuard,
    _quarantine_guard: DirectoryIdentityGuard,
}

fn recovery_path_changed() -> ApiError {
    ApiError::new(
        "path_changed",
        "The recovery directory changed after InkFlow opened it.",
    )
}

fn validate_checkpoint_request(request: &CheckpointRequest) -> ApiResult<()> {
    if request.content.len() > MAX_RECOVERY_CONTENT_BYTES {
        return Err(ApiError::new(
            "recovery_too_large",
            "The document is larger than the 32 MiB recovery snapshot limit.",
        ));
    }
    Ok(())
}

fn is_managed_quarantine_file_name(file_name: &str) -> bool {
    let Some(base) = file_name.strip_suffix(".invalid") else {
        return false;
    };
    if base.ends_with(".zst") {
        return true;
    }
    let Some((record_name, suffix)) = base.rsplit_once(".zst-") else {
        return false;
    };
    !record_name.is_empty() && Uuid::parse_str(suffix).is_ok()
}

impl RecoveryStore {
    pub fn new(directory: PathBuf) -> ApiResult<Self> {
        fs::create_dir_all(&directory)
            .map_err(|error| ApiError::io("Unable to create the recovery directory", error))?;
        if is_symbolic_link_or_junction(&directory)? {
            return Err(ApiError::new(
                "path_changed",
                "The recovery directory cannot be a symbolic link or directory junction.",
            ));
        }
        let directory = canonical_existing(&directory)?;
        if !directory.is_dir() {
            return Err(ApiError::new(
                "path_changed",
                "The recovery path is no longer a directory.",
            ));
        }
        let directory_identity = directory_identity(&directory)?;
        Ok(Self {
            directory,
            directory_identity,
            last_checkpoint: Mutex::new(HashMap::new()),
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn guard_directory(&self) -> ApiResult<DirectoryIdentityGuard> {
        let current = canonical_existing(&self.directory).map_err(|_| recovery_path_changed())?;
        if current != self.directory
            || is_symbolic_link_or_junction(&self.directory).map_err(|_| recovery_path_changed())?
        {
            return Err(recovery_path_changed());
        }
        guard_directory_identity(&current, self.directory_identity)
            .map_err(|_| recovery_path_changed())
    }

    pub fn checkpoint(&self, request: CheckpointRequest) -> ApiResult<Option<RecoveryEntry>> {
        let _directory_guard = self.guard_directory()?;
        validate_checkpoint_request(&request)?;
        let lock = DataLock::acquire(&self.directory.join(".recovery.lock"))?;
        self.checkpoint_locked(request, lock)
    }

    /// Writes a save-adjacent checkpoint only when the recovery store is
    /// immediately available. Explicit recovery operations use `checkpoint`
    /// and retain their bounded waiting behavior.
    pub fn try_checkpoint(&self, request: CheckpointRequest) -> ApiResult<bool> {
        let _directory_guard = self.guard_directory()?;
        validate_checkpoint_request(&request)?;
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
        let encoder = zstd::stream::write::Encoder::new(Vec::new(), 3)
            .map_err(|error| ApiError::io("Unable to start recovery compression", error))?;
        let mut counted = CountingWriter::new(encoder, MAX_RECOVERY_RECORD_BYTES as u64);
        let serialization = serde_json::to_writer(&mut counted, &record);
        if counted.limit_exceeded() {
            return Err(ApiError::new(
                "recovery_too_large",
                "The recovery snapshot expands beyond its safety limit.",
            ));
        }
        serialization.map_err(|error| ApiError::new("recovery_error", error.to_string()))?;
        let (encoder, uncompressed_bytes) = counted.into_parts();
        let compressed = encoder
            .finish()
            .map_err(|error| ApiError::io("Unable to compress the recovery snapshot", error))?;
        if compressed.len() as u64 > MAX_COMPRESSED_RECORD_BYTES {
            return Err(ApiError::new(
                "recovery_too_large",
                "The compressed recovery snapshot exceeds its safety limit.",
            ));
        }
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
            uncompressed_bytes,
        });
        self.cleanup_index_preserving(&mut index, Some(&entry.id));
        self.cleanup_quarantine(active_compressed_bytes(&index));
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
        let _directory_guard = self.guard_directory()?;
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
        let _directory_guard = self.guard_directory()?;
        let _lock = DataLock::acquire(&self.directory.join(".recovery.lock"))?;
        let mut index = self.load_or_rebuild_index()?;
        if let Some(indexed) = index
            .records
            .iter()
            .find(|record| record.entry.id == id)
            .cloned()
        {
            let path = self.directory.join(&indexed.file_name);
            match self.read_record(&path) {
                Ok((record, _)) => {
                    return Ok(RecoverySnapshot {
                        entry: record.entry,
                        content: record.content,
                    });
                }
                Err(error) => {
                    // A fingerprint is only an acceleration hint. Bit rot or a
                    // restored timestamp can leave it unchanged even though the
                    // record no longer validates. Remove a successfully
                    // quarantined record from the active index immediately so
                    // it cannot stay listed or consume normal-history quota.
                    if self.quarantine_record(&path).is_ok() {
                        index
                            .records
                            .retain(|record| record.file_name != indexed.file_name);
                        index
                            .files
                            .retain(|file| file.file_name != indexed.file_name);
                        self.cleanup_quarantine(active_compressed_bytes(&index));
                        let _ = self.persist_index_if_complete(&index);
                    }
                    return Err(error);
                }
            }
        }
        Err(ApiError::new(
            "recovery_not_found",
            "The requested recovery snapshot no longer exists.",
        ))
    }

    pub fn delete(&self, id: &str) -> ApiResult<bool> {
        let _directory_guard = self.guard_directory()?;
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
        let _directory_guard = self.guard_directory()?;
        let lock = DataLock::acquire(&self.directory.join(".recovery.lock"))?;
        self.delete_document_kind_locked(document_id, kind, lock)
    }

    pub fn try_delete_document_kind(&self, document_id: &str, kind: &str) -> ApiResult<bool> {
        let _directory_guard = self.guard_directory()?;
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

    fn read_record(&self, path: &Path) -> ApiResult<(RecoveryRecord, u64)> {
        self.read_record_with_limit(path, MAX_RECOVERY_RECORD_BYTES as u64)
            .map_err(|(error, _)| error)
    }

    fn read_record_with_limit(
        &self,
        path: &Path,
        max_uncompressed: u64,
    ) -> Result<(RecoveryRecord, u64), (ApiError, u64)> {
        let metadata = fs::metadata(path).map_err(|error| {
            (
                ApiError::io("Unable to inspect a recovery snapshot", error),
                0,
            )
        })?;
        if metadata.len() > MAX_COMPRESSED_RECORD_BYTES {
            return Err((
                ApiError::new(
                    "recovery_too_large",
                    "The compressed recovery snapshot exceeds its safety limit.",
                ),
                0,
            ));
        }
        let max_uncompressed = max_uncompressed.min(MAX_RECOVERY_RECORD_BYTES as u64);
        if max_uncompressed == 0 {
            return Err((
                ApiError::new(
                    "recovery_too_large",
                    "The recovery rebuild reached its uncompressed safety limit.",
                ),
                0,
            ));
        }
        let file = fs::File::open(path)
            .map_err(|error| (ApiError::io("Unable to read a recovery snapshot", error), 0))?;
        let mut decoder = zstd::stream::read::Decoder::new(file).map_err(|error| {
            (
                ApiError::io("Unable to decompress a recovery snapshot", error),
                0,
            )
        })?;
        decoder
            .window_log_max(MAX_RECOVERY_WINDOW_LOG)
            .map_err(|error| {
                (
                    ApiError::io("Unable to limit recovery decompression", error),
                    0,
                )
            })?;
        let mut json = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(MAX_RECOVERY_RECORD_BYTES)
                .min(MAX_RECOVERY_RECORD_BYTES)
                .min(usize::try_from(max_uncompressed).unwrap_or(MAX_RECOVERY_RECORD_BYTES)),
        );
        decoder
            .take(max_uncompressed.saturating_add(1))
            .read_to_end(&mut json)
            .map_err(|error| {
                (
                    ApiError::io("Unable to decompress a recovery snapshot", error),
                    json.len() as u64,
                )
            })?;
        if json.len() as u64 > max_uncompressed {
            return Err((
                ApiError::new(
                    "recovery_too_large",
                    "The recovery snapshot expands beyond its safety limit.",
                ),
                json.len() as u64,
            ));
        }
        let record = serde_json::from_slice::<RecoveryRecord>(&json).map_err(|error| {
            (
                ApiError::new("recovery_error", error.to_string()),
                json.len() as u64,
            )
        })?;
        let actual_size = record.content.len();
        if actual_size > MAX_RECOVERY_CONTENT_BYTES
            || record.entry.size != actual_size
            || record.hash != blake3::hash(record.content.as_bytes()).to_hex().as_str()
        {
            return Err((
                ApiError::new(
                    "recovery_corrupt",
                    "The recovery snapshot failed its content integrity check.",
                ),
                json.len() as u64,
            ));
        }
        Ok((record, json.len() as u64))
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

    fn quarantine_record(&self, path: &Path) -> ApiResult<()> {
        let quarantine = self.quarantine_directory(true)?.ok_or_else(|| {
            ApiError::new(
                "recovery_error",
                "The recovery quarantine directory is unavailable.",
            )
        })?;
        let directory = &quarantine.path;
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| ApiError::new("recovery_error", "A recovery filename is invalid."))?;
        let mut destination = directory.join(format!("{file_name}.invalid"));
        if destination.exists() {
            destination = directory.join(format!("{file_name}-{}.invalid", Uuid::new_v4()));
        }
        fs::rename(path, destination)
            .map_err(|error| ApiError::io("Unable to quarantine a recovery snapshot", error))
    }

    fn quarantine_directory(&self, create: bool) -> ApiResult<Option<GuardedQuarantineDirectory>> {
        let recovery_guard = self.guard_directory()?;
        let recovery = self.directory.clone();
        let candidate = recovery.join(QUARANTINE_DIRECTORY);

        match fs::symlink_metadata(&candidate) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => {
                return Ok(None);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&candidate) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(ApiError::io("Unable to create recovery quarantine", error));
                    }
                }
            }
            Err(error) => {
                return Err(ApiError::io("Unable to inspect recovery quarantine", error));
            }
        }

        if is_symbolic_link_or_junction(&candidate)? {
            return Err(ApiError::new(
                "path_changed",
                "The recovery quarantine cannot be a symbolic link or directory junction.",
            ));
        }
        let directory = canonical_existing(&candidate)?;
        if !directory.is_dir() || directory.parent() != Some(recovery.as_path()) {
            return Err(ApiError::new(
                "path_changed",
                "The recovery quarantine is outside the recovery directory.",
            ));
        }
        let quarantine_identity = directory_identity(&directory)?;
        let quarantine_guard = guard_directory_identity(&directory, quarantine_identity)?;

        // Revalidate the user-visible child path while both directory handles
        // are held. This closes the check/use gap on Windows if an attacker
        // attempts to replace Quarantine with a junction during validation.
        if is_symbolic_link_or_junction(&candidate)? || canonical_existing(&candidate)? != directory
        {
            return Err(ApiError::new(
                "path_changed",
                "The recovery quarantine changed during validation.",
            ));
        }

        Ok(Some(GuardedQuarantineDirectory {
            path: directory,
            _recovery_guard: recovery_guard,
            _quarantine_guard: quarantine_guard,
        }))
    }

    fn cleanup_quarantine(&self, active_compressed: u64) {
        let Ok(Some(quarantine)) = self.quarantine_directory(false) else {
            return;
        };
        let Ok(entries) = fs::read_dir(&quarantine.path) else {
            return;
        };
        let maximum_age = Duration::from_secs(MAX_AGE_DAYS as u64 * 24 * 60 * 60);
        let now = SystemTime::now();
        let mut retained = Vec::new();
        for entry in entries.flatten() {
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_managed_quarantine_file_name(&file_name) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
            let expired = now
                .duration_since(modified)
                .is_ok_and(|elapsed| elapsed > maximum_age);
            if expired && fs::remove_file(&path).is_ok() {
                continue;
            }
            retained.push((path, metadata.len(), modified));
        }
        retained.sort_by_key(|(_, _, modified)| *modified);
        let mut total = retained
            .iter()
            .fold(active_compressed, |sum, (_, bytes, _)| {
                sum.saturating_add(*bytes)
            });
        for (path, bytes, _) in retained {
            if total <= MAX_TOTAL_COMPRESSED_BYTES {
                break;
            }
            if fs::remove_file(path).is_ok() {
                total = total.saturating_sub(bytes);
            }
        }
    }

    fn load_or_rebuild_index(&self) -> ApiResult<RecoveryIndexV2> {
        self.load_or_rebuild_index_with_limits(
            MAX_TOTAL_COMPRESSED_BYTES,
            MAX_TOTAL_UNCOMPRESSED_BYTES,
        )
    }

    fn load_or_rebuild_index_with_limits(
        &self,
        max_total_compressed: u64,
        max_total_uncompressed: u64,
    ) -> ApiResult<RecoveryIndexV2> {
        let mut paths = self.record_files()?;
        // Managed filenames begin with their creation timestamp. Rebuild those
        // newest-first and leave unknown names until last, so normal InkFlow
        // history is considered before foreign files.
        paths.sort_by(|left, right| {
            match (
                recovery_file_timestamp(left),
                recovery_file_timestamp(right),
            ) {
                (Some(left), Some(right)) => right.cmp(&left),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => right.file_name().cmp(&left.file_name()),
            }
        });
        let fingerprints = recovery_fingerprints(&paths)?;
        if let Ok(bytes) = fs::read(self.directory.join(RECOVERY_INDEX_FILE))
            && let Ok(index) = serde_json::from_slice::<RecoveryIndexV2>(&bytes)
            && recovery_index_matches(&index, &fingerprints)
        {
            self.cleanup_quarantine(active_compressed_bytes(&index));
            return Ok(index);
        }

        let mut index = RecoveryIndexV2::default();
        let mut rebuild_complete = true;
        let cutoff = Utc::now() - chrono::Duration::days(MAX_AGE_DAYS);
        let mut retained_compressed = 0u64;
        let mut retained_uncompressed = 0u64;
        let mut retained_per_document = HashMap::<String, usize>::new();
        for path in paths {
            let compressed_bytes = match fs::metadata(&path) {
                Ok(metadata) => metadata.len(),
                Err(_) => {
                    if self.quarantine_record(&path).is_err() {
                        rebuild_complete = false;
                    }
                    continue;
                }
            };
            if compressed_bytes > MAX_COMPRESSED_RECORD_BYTES {
                if self.quarantine_record(&path).is_err() {
                    rebuild_complete = false;
                }
                continue;
            }
            let (record, uncompressed_bytes) = match self.read_record(&path) {
                Ok(decoded) => decoded,
                Err(_) => {
                    // Invalid records are bounded individually, then removed from
                    // the active set. Their decoded bytes must not spend the quota
                    // reserved for valid history that appears later in the scan.
                    if self.quarantine_record(&path).is_err() {
                        rebuild_complete = false;
                    }
                    continue;
                }
            };
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let indexed = RecoveryIndexRecord {
                file_name: file_name.to_string(),
                entry: record.entry,
                hash: record.hash,
                compressed_bytes,
                uncompressed_bytes,
            };
            retained_compressed = retained_compressed.saturating_add(compressed_bytes);
            retained_uncompressed = retained_uncompressed.saturating_add(uncompressed_bytes);
            let per_document = retained_per_document
                .entry(indexed.entry.document_id.clone())
                .or_default();
            *per_document += 1;
            let needs_cleanup = recovery_created_at(&indexed) < cutoff
                || *per_document > MAX_PER_DOCUMENT
                || retained_compressed > max_total_compressed
                || retained_uncompressed > max_total_uncompressed;
            index.records.push(indexed);

            // Do not materialize an arbitrarily large candidate index and only
            // apply the aggregate limits afterwards. A directory containing
            // many individually valid, highly compressed records must remain
            // bounded throughout reconstruction as well as in the final index.
            if needs_cleanup {
                rebuild_complete &= self.cleanup_rebuilt_records_with_limits(
                    &mut index,
                    MAX_PER_DOCUMENT,
                    max_total_compressed,
                    max_total_uncompressed,
                );
                (
                    retained_compressed,
                    retained_uncompressed,
                    retained_per_document,
                ) = recovery_index_usage(&index.records);
            }
        }
        rebuild_complete &= self.cleanup_rebuilt_records_with_limits(
            &mut index,
            MAX_PER_DOCUMENT,
            max_total_compressed,
            max_total_uncompressed,
        );
        index.files = recovery_fingerprints(&self.record_files()?)?;
        if !rebuild_complete {
            let valid_names = index
                .records
                .iter()
                .map(|record| record.file_name.as_str())
                .collect::<HashSet<_>>();
            index
                .files
                .retain(|file| valid_names.contains(file.file_name.as_str()));
        }
        self.cleanup_index_preserving(&mut index, None);
        self.cleanup_quarantine(active_compressed_bytes(&index));
        // Reads must remain available even if a damaged or read-only data
        // directory prevents this optional acceleration index from persisting.
        if rebuild_complete {
            let _ = self.persist_index_if_complete(&index);
        }
        Ok(index)
    }

    fn cleanup_rebuilt_records_with_limits(
        &self,
        index: &mut RecoveryIndexV2,
        max_per_document: usize,
        max_compressed: u64,
        max_uncompressed: u64,
    ) -> bool {
        let cutoff = Utc::now() - chrono::Duration::days(MAX_AGE_DAYS);
        index
            .records
            .sort_by(|left, right| recovery_created_at(right).cmp(&recovery_created_at(left)));
        let mut complete = true;
        let mut per_document = HashMap::<String, usize>::new();
        let mut compressed = 0u64;
        let mut uncompressed = 0u64;
        let mut retained = Vec::with_capacity(index.records.len());
        for record in std::mem::take(&mut index.records) {
            let count = per_document
                .entry(record.entry.document_id.clone())
                .or_default();
            let should_prune = recovery_created_at(&record) < cutoff
                || *count >= max_per_document
                || compressed.saturating_add(record.compressed_bytes) > max_compressed
                || uncompressed.saturating_add(record.uncompressed_bytes) > max_uncompressed;
            if should_prune {
                let path = self.directory.join(&record.file_name);
                match fs::remove_file(&path) {
                    Ok(()) => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        // A record that remains on disk must continue to count
                        // against every quota. Mark the rebuild incomplete so
                        // a later operation retries the failed deletion.
                        complete = false;
                    }
                }
            }
            *count += 1;
            compressed = compressed.saturating_add(record.compressed_bytes);
            uncompressed = uncompressed.saturating_add(record.uncompressed_bytes);
            retained.push(record);
        }
        index.records = retained;
        complete
    }

    fn persist_index_if_complete(&self, index: &RecoveryIndexV2) -> ApiResult<()> {
        if recovery_index_is_structurally_complete(index) {
            self.persist_index(index)?;
        }
        Ok(())
    }

    fn persist_index(&self, index: &RecoveryIndexV2) -> ApiResult<()> {
        let mut persisted = index.clone();
        persisted.integrity = recovery_index_integrity(&persisted)?;
        let bytes = serde_json::to_vec(&persisted)
            .map_err(|error| ApiError::new("recovery_error", error.to_string()))?;
        atomic_write(&self.directory.join(RECOVERY_INDEX_FILE), &bytes)?;
        let _ = fs::remove_file(self.directory.join(LEGACY_RECOVERY_INDEX_FILE));
        Ok(())
    }

    #[cfg(test)]
    fn cleanup_index(&self, index: &mut RecoveryIndexV2) {
        self.cleanup_index_preserving(index, None);
    }

    fn cleanup_index_preserving(
        &self,
        index: &mut RecoveryIndexV2,
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
        let mut total_compressed = 0u64;
        let mut total_uncompressed = 0u64;
        for file in &index.files {
            if valid_names.contains(&file.file_name) {
                continue;
            }
            // A missing index record can mean the snapshot was temporarily
            // unreadable. Keep it for a later rebuild instead of treating an
            // acceleration-index miss as authority to delete recovery data.
            retained_unreadable.insert(file.file_name.clone());
            total_compressed = total_compressed.saturating_add(file.bytes);
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
                    || total_compressed.saturating_add(record.compressed_bytes)
                        > MAX_TOTAL_COMPRESSED_BYTES
                    || total_uncompressed.saturating_add(record.uncompressed_bytes)
                        > MAX_TOTAL_UNCOMPRESSED_BYTES);
            if should_prune && fs::remove_file(self.directory.join(&record.file_name)).is_ok() {
                continue;
            }
            *count += 1;
            total_compressed = total_compressed.saturating_add(record.compressed_bytes);
            total_uncompressed = total_uncompressed.saturating_add(record.uncompressed_bytes);
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
    index: &'a RecoveryIndexV2,
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

fn recovery_index_is_structurally_complete(index: &RecoveryIndexV2) -> bool {
    if index.schema_version != 2 || index.files.len() != index.records.len() {
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
    if file_names.len() != index.files.len()
        || record_names.len() != index.records.len()
        || file_names != record_names
    {
        return false;
    }
    let compressed_bytes = index
        .files
        .iter()
        .map(|file| (file.file_name.as_str(), file.bytes))
        .collect::<HashMap<_, _>>();
    index.records.iter().all(|record| {
        compressed_bytes.get(record.file_name.as_str()) == Some(&record.compressed_bytes)
            && record.compressed_bytes > 0
            && record.compressed_bytes <= MAX_COMPRESSED_RECORD_BYTES
            && record.uncompressed_bytes > 0
            && record.uncompressed_bytes <= MAX_RECOVERY_RECORD_BYTES as u64
            && record.entry.size <= MAX_RECOVERY_CONTENT_BYTES
            && record.uncompressed_bytes >= record.entry.size as u64
    })
}

fn recovery_index_integrity(index: &RecoveryIndexV2) -> ApiResult<String> {
    let payload = serde_json::to_vec(&(index.schema_version, &index.files, &index.records))
        .map_err(|error| ApiError::new("recovery_error", error.to_string()))?;
    Ok(blake3::hash(&payload).to_hex().to_string())
}

fn recovery_index_is_complete(index: &RecoveryIndexV2) -> bool {
    recovery_index_is_structurally_complete(index)
        && !index.integrity.is_empty()
        && recovery_index_integrity(index).is_ok_and(|integrity| integrity == index.integrity)
}

fn recovery_index_matches(
    index: &RecoveryIndexV2,
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

fn recovery_file_timestamp(path: &Path) -> Option<i64> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".json.zst")?;
    let (timestamp, id) = stem.split_once('-')?;
    Uuid::parse_str(id).ok()?;
    timestamp.parse().ok()
}

fn active_compressed_bytes(index: &RecoveryIndexV2) -> u64 {
    index.records.iter().fold(0u64, |total, record| {
        total.saturating_add(record.compressed_bytes)
    })
}

fn recovery_index_usage(records: &[RecoveryIndexRecord]) -> (u64, u64, HashMap<String, usize>) {
    let mut compressed = 0u64;
    let mut uncompressed = 0u64;
    let mut per_document = HashMap::<String, usize>::new();
    for record in records {
        compressed = compressed.saturating_add(record.compressed_bytes);
        uncompressed = uncompressed.saturating_add(record.uncompressed_bytes);
        *per_document
            .entry(record.entry.document_id.clone())
            .or_default() += 1;
    }
    (compressed, uncompressed, per_document)
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

    fn synthetic_index_record(
        file_name: &str,
        document_id: &str,
        created_at: DateTime<Utc>,
        compressed_bytes: u64,
        uncompressed_bytes: u64,
    ) -> RecoveryIndexRecord {
        RecoveryIndexRecord {
            file_name: file_name.into(),
            entry: RecoveryEntry {
                id: Uuid::new_v4().to_string(),
                document_id: document_id.into(),
                path: None,
                title: document_id.into(),
                created_at: created_at.to_rfc3339(),
                kind: "history".into(),
                size: 1,
            },
            hash: "synthetic".into(),
            compressed_bytes,
            uncompressed_bytes,
        }
    }

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
    fn rebuild_reader_stops_at_the_remaining_aggregate_budget() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        store
            .checkpoint(CheckpointRequest {
                document_id: "bounded-rebuild".into(),
                path: None,
                title: "Bounded rebuild".into(),
                content: "content that exceeds a tiny remaining budget".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let record_path = store.record_files().unwrap().pop().unwrap();

        let (error, decoded_bytes) = store.read_record_with_limit(&record_path, 16).unwrap_err();

        assert_eq!(error.code, "recovery_too_large");
        assert_eq!(decoded_bytes, 17);
    }

    #[test]
    fn rebuild_cleanup_applies_aggregate_limits_before_retaining_all_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let newest_name = "newest.json.zst";
        let oldest_name = "oldest.json.zst";
        fs::write(temp.path().join(newest_name), b"newest").unwrap();
        fs::write(temp.path().join(oldest_name), b"oldest").unwrap();
        let mut index = RecoveryIndexV2 {
            records: vec![
                synthetic_index_record(newest_name, "newest", Utc::now(), 6, 6),
                synthetic_index_record(
                    oldest_name,
                    "oldest",
                    Utc::now() - chrono::Duration::minutes(1),
                    6,
                    6,
                ),
            ],
            ..RecoveryIndexV2::default()
        };

        assert!(store.cleanup_rebuilt_records_with_limits(&mut index, 50, 10, 10));

        assert_eq!(index.records.len(), 1);
        assert_eq!(index.records[0].file_name, newest_name);
        assert!(temp.path().join(newest_name).is_file());
        assert!(!temp.path().join(oldest_name).exists());
    }

    #[test]
    fn rebuild_cleanup_counts_records_that_cannot_be_deleted() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let newest_name = "newest.json.zst";
        let locked_name = "locked.json.zst";
        let other_name = "other.json.zst";
        fs::write(temp.path().join(newest_name), b"newest").unwrap();
        // remove_file deterministically fails for a directory on every
        // supported platform, which models a snapshot locked by another
        // process without relying on platform-specific sharing flags.
        fs::create_dir(temp.path().join(locked_name)).unwrap();
        fs::write(temp.path().join(other_name), b"other").unwrap();
        let now = Utc::now();
        let mut index = RecoveryIndexV2 {
            records: vec![
                synthetic_index_record(newest_name, "document-a", now, 40, 40),
                synthetic_index_record(
                    locked_name,
                    "document-a",
                    now - chrono::Duration::seconds(1),
                    40,
                    40,
                ),
                synthetic_index_record(
                    other_name,
                    "document-b",
                    now - chrono::Duration::seconds(2),
                    50,
                    50,
                ),
            ],
            ..RecoveryIndexV2::default()
        };

        assert!(!store.cleanup_rebuilt_records_with_limits(&mut index, 1, 100, 100));

        assert_eq!(
            index
                .records
                .iter()
                .map(|record| record.file_name.as_str())
                .collect::<Vec<_>>(),
            vec![newest_name, locked_name]
        );
        assert!(temp.path().join(newest_name).is_file());
        assert!(temp.path().join(locked_name).is_dir());
        assert!(!temp.path().join(other_name).exists());
    }

    #[test]
    fn restore_quarantines_a_record_that_fails_after_its_index_was_validated() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let entry = store
            .checkpoint(CheckpointRequest {
                document_id: "restore-corruption".into(),
                path: None,
                title: "Restore corruption".into(),
                content: "recoverable content".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let mut index = store.load_or_rebuild_index().unwrap();
        let record_path = temp.path().join(&index.records[0].file_name);
        let compressed_len = fs::metadata(&record_path).unwrap().len() as usize;
        fs::write(&record_path, vec![0; compressed_len]).unwrap();
        index.files = recovery_fingerprints(&store.record_files().unwrap()).unwrap();
        store.persist_index(&index).unwrap();

        assert!(store.restore(&entry.id).is_err());
        assert!(!record_path.exists());
        assert!(store.list().unwrap().is_empty());
        assert_eq!(
            fs::read_dir(temp.path().join(QUARANTINE_DIRECTORY))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn rejects_content_above_the_recovery_limit_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let error = store
            .checkpoint(CheckpointRequest {
                document_id: "oversized".into(),
                path: None,
                title: "Oversized".into(),
                content: "x".repeat(MAX_RECOVERY_CONTENT_BYTES + 1),
                kind: Some("draft".into()),
            })
            .unwrap_err();

        assert_eq!(error.code, "recovery_too_large");
        assert!(store.record_files().unwrap().is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn try_checkpoint_reports_oversized_content_while_the_store_is_busy() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let _recovery_lock = DataLock::acquire(&store.directory().join(".recovery.lock")).unwrap();

        let error = store
            .try_checkpoint(CheckpointRequest {
                document_id: "oversized-busy".into(),
                path: None,
                title: "Oversized while busy".into(),
                content: "x".repeat(MAX_RECOVERY_CONTENT_BYTES + 1),
                kind: Some("draft".into()),
            })
            .unwrap_err();

        assert_eq!(error.code, "recovery_too_large");
        assert!(store.record_files().unwrap().is_empty());
    }

    #[test]
    fn stops_streaming_json_as_soon_as_escaping_exceeds_the_record_limit() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        // NUL is serialized as six JSON bytes. This content is well below the
        // 32 MiB body limit but expands beyond the 33 MiB record boundary.
        let escaped_content = "\0".repeat(MAX_RECOVERY_RECORD_BYTES / 6 + 1);

        let error = store
            .checkpoint(CheckpointRequest {
                document_id: "escaped-oversized".into(),
                path: None,
                title: "Escaped oversized".into(),
                content: escaped_content,
                kind: Some("draft".into()),
            })
            .unwrap_err();

        assert_eq!(error.code, "recovery_too_large");
        assert!(store.record_files().unwrap().is_empty());
    }

    #[test]
    fn index_records_the_actual_decompressed_json_size() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let entry = store
            .checkpoint(CheckpointRequest {
                document_id: "measured".into(),
                path: Some("C:\\notes\\measured.md".into()),
                title: "Measured".into(),
                content: "small content".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let index = serde_json::from_slice::<RecoveryIndexV2>(
            &fs::read(temp.path().join(RECOVERY_INDEX_FILE)).unwrap(),
        )
        .unwrap();

        assert!(index.records[0].uncompressed_bytes > entry.size as u64);
        assert!(index.records[0].uncompressed_bytes <= MAX_RECOVERY_RECORD_BYTES as u64);
    }

    #[test]
    fn bounded_decoder_quarantines_a_high_compression_ratio_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let expanded = vec![b'x'; MAX_RECOVERY_RECORD_BYTES + 1];
        let compressed = zstd::stream::encode_all(expanded.as_slice(), 1).unwrap();
        let path = temp.path().join("compression-bomb.json.zst");
        fs::write(&path, compressed).unwrap();

        assert_eq!(
            store.read_record(&path).unwrap_err().code,
            "recovery_too_large"
        );
        assert!(store.list().unwrap().is_empty());
        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(temp.path().join(QUARANTINE_DIRECTORY))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn decoder_rejects_frames_with_an_oversized_window() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).unwrap();
        encoder.window_log(MAX_RECOVERY_WINDOW_LOG + 1).unwrap();
        encoder.write_all(b"{}\n").unwrap();
        let compressed = encoder.finish().unwrap();
        let path = temp.path().join("oversized-window.json.zst");
        fs::write(&path, compressed).unwrap();

        assert!(store.list().unwrap().is_empty());
        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(temp.path().join(QUARANTINE_DIRECTORY))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn oversized_compressed_records_are_quarantined_without_decoding() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let path = temp.path().join("oversized-compressed.json.zst");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_COMPRESSED_RECORD_BYTES + 1).unwrap();
        drop(file);

        assert!(store.list().unwrap().is_empty());
        assert!(!path.exists());
        let quarantined = fs::read_dir(temp.path().join(QUARANTINE_DIRECTORY))
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(
            quarantined.metadata().unwrap().len(),
            MAX_COMPRESSED_RECORD_BYTES + 1
        );
    }

    #[test]
    fn corrupt_records_are_quarantined_without_displacing_valid_history() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let valid = store
            .checkpoint(CheckpointRequest {
                document_id: "valid".into(),
                path: None,
                title: "Valid".into(),
                content: "recover me".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        fs::write(temp.path().join("broken.json.zst"), b"not a zstd frame").unwrap();

        let listed = store.list().unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, valid.id);
        assert_eq!(
            fs::read_dir(temp.path().join(QUARANTINE_DIRECTORY))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn corrupt_decoded_bytes_do_not_consume_valid_rebuild_quota() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let valid = store
            .checkpoint(CheckpointRequest {
                document_id: "valid-after-corrupt".into(),
                path: None,
                title: "Valid after corrupt".into(),
                content: "recover this content".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let index_path = temp.path().join(RECOVERY_INDEX_FILE);
        let original =
            serde_json::from_slice::<RecoveryIndexV2>(&fs::read(&index_path).unwrap()).unwrap();
        let valid_record = original.records[0].clone();
        let valid_path = temp.path().join(&valid_record.file_name);
        let corrupt_bytes = zstd::stream::encode_all(
            std::io::Cursor::new(vec![b'x'; valid_record.uncompressed_bytes as usize]),
            3,
        )
        .unwrap();
        let corrupt_path = temp.path().join(format!(
            "{}-{}.json.zst",
            Utc::now().timestamp_millis() + 60_000,
            Uuid::new_v4()
        ));
        fs::write(&corrupt_path, corrupt_bytes).unwrap();
        fs::remove_file(index_path).unwrap();

        let rebuilt = store
            .load_or_rebuild_index_with_limits(
                MAX_TOTAL_COMPRESSED_BYTES,
                valid_record.uncompressed_bytes,
            )
            .unwrap();

        assert_eq!(rebuilt.records.len(), 1);
        assert_eq!(rebuilt.records[0].entry.id, valid.id);
        assert!(valid_path.is_file());
        assert!(!corrupt_path.exists());
    }

    #[test]
    fn migrates_legacy_indexes_only_after_a_bounded_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let entry = store
            .checkpoint(CheckpointRequest {
                document_id: "legacy".into(),
                path: None,
                title: "Legacy".into(),
                content: "legacy content".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let current = temp.path().join(RECOVERY_INDEX_FILE);
        let legacy = temp.path().join(LEGACY_RECOVERY_INDEX_FILE);
        fs::rename(&current, &legacy).unwrap();

        let reopened = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(reopened.list().unwrap()[0].id, entry.id);
        assert!(current.is_file());
        assert!(!legacy.exists());
        let migrated =
            serde_json::from_slice::<RecoveryIndexV2>(&fs::read(current).unwrap()).unwrap();
        assert_eq!(migrated.schema_version, 2);
        assert!(migrated.records[0].uncompressed_bytes > entry.size as u64);
    }

    #[test]
    fn cleanup_enforces_compressed_and_uncompressed_quotas_independently() {
        for use_compressed_quota in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
            let now = Utc::now();
            let mut index = RecoveryIndexV2::default();
            for sequence in 0..3 {
                let file_name = format!("quota-{sequence}.json.zst");
                fs::write(temp.path().join(&file_name), b"record").unwrap();
                index.records.push(synthetic_index_record(
                    &file_name,
                    &format!("document-{sequence}"),
                    now - chrono::Duration::seconds(sequence),
                    if use_compressed_quota {
                        260 * 1024 * 1024
                    } else {
                        1
                    },
                    if use_compressed_quota {
                        1
                    } else {
                        260 * 1024 * 1024
                    },
                ));
            }
            index.files = recovery_fingerprints(&store.record_files().unwrap()).unwrap();

            store.cleanup_index(&mut index);

            assert_eq!(index.records.len(), 1);
            assert_eq!(store.record_files().unwrap().len(), 1);
        }
    }

    #[test]
    fn quarantine_is_pruned_before_active_history_when_disk_quota_is_exceeded() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let active = store
            .checkpoint(CheckpointRequest {
                document_id: "active".into(),
                path: None,
                title: "Active".into(),
                content: "active content".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let quarantine = temp.path().join(QUARANTINE_DIRECTORY);
        fs::create_dir_all(&quarantine).unwrap();
        let oversized = quarantine.join("old.json.zst.invalid");
        let file = fs::File::create(&oversized).unwrap();
        file.set_len(MAX_TOTAL_COMPRESSED_BYTES).unwrap();
        drop(file);
        let unmanaged = quarantine.join("keep-user-file.txt");
        fs::write(&unmanaged, b"not owned by InkFlow quarantine cleanup").unwrap();
        let index = store.load_or_rebuild_index().unwrap();

        store.cleanup_quarantine(active_compressed_bytes(&index));

        assert!(!oversized.exists());
        assert!(unmanaged.exists());
        assert_eq!(store.restore(&active.id).unwrap().content, "active content");
    }

    #[test]
    fn quarantine_must_be_a_real_directory() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        fs::write(temp.path().join(QUARANTINE_DIRECTORY), b"not a directory").unwrap();

        let error = store.quarantine_directory(false).err().unwrap();

        assert_eq!(error.code, "path_changed");
    }

    #[test]
    fn replaced_recovery_root_is_rejected_before_lock_or_quarantine() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        let displaced = temp.path().join("Recovery-original");
        let store = RecoveryStore::new(recovery.clone()).unwrap();
        fs::rename(&recovery, &displaced).unwrap();
        fs::create_dir(&recovery).unwrap();
        let external_record = recovery.join("external.json.zst");
        fs::write(&external_record, b"not an InkFlow recovery record").unwrap();

        let error = store.list().unwrap_err();

        assert_eq!(error.code, "path_changed");
        assert!(external_record.exists());
        assert!(!recovery.join(".recovery.lock").exists());
        assert!(!recovery.join(QUARANTINE_DIRECTORY).exists());
    }

    #[test]
    fn quarantine_cleanup_does_not_follow_a_directory_link() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let external_record = outside.join("external.json.zst.invalid");
        fs::write(&external_record, b"must remain outside Recovery").unwrap();
        let store = RecoveryStore::new(recovery.clone()).unwrap();
        let quarantine = recovery.join(QUARANTINE_DIRECTORY);

        #[cfg(target_os = "windows")]
        let linked = std::os::windows::fs::symlink_dir(&outside, &quarantine).is_ok();
        #[cfg(not(target_os = "windows"))]
        let linked = std::os::unix::fs::symlink(&outside, &quarantine).is_ok();
        if !linked {
            // Windows requires developer mode or the symlink privilege. The
            // path validation itself is still covered by the real-file test.
            return;
        }

        store.cleanup_quarantine(MAX_TOTAL_COMPRESSED_BYTES);

        assert!(external_record.exists());
        assert_eq!(
            store.quarantine_directory(false).err().unwrap().code,
            "path_changed"
        );
    }

    #[test]
    fn quarantine_cleanup_recognizes_only_owned_file_names() {
        assert!(is_managed_quarantine_file_name("record.json.zst.invalid"));
        assert!(is_managed_quarantine_file_name(&format!(
            "record.json.zst-{}.invalid",
            Uuid::new_v4()
        )));
        assert!(!is_managed_quarantine_file_name("record.invalid"));
        assert!(!is_managed_quarantine_file_name(
            "record.json.zst-not-a-uuid.invalid"
        ));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn failed_quarantine_does_not_displace_valid_history() {
        use std::os::windows::fs::OpenOptionsExt;

        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;

        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let valid = store
            .checkpoint(CheckpointRequest {
                document_id: "valid".into(),
                path: None,
                title: "Valid".into(),
                content: "keep this history".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let invalid = temp.path().join("locked-invalid.json.zst");
        let invalid_file = fs::File::create(&invalid).unwrap();
        invalid_file.set_len(MAX_TOTAL_COMPRESSED_BYTES).unwrap();
        drop(invalid_file);
        let locked = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&invalid)
            .unwrap();

        let listed = store.list().unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, valid.id);
        assert!(store.restore(&valid.id).is_ok());
        drop(locked);

        assert_eq!(store.list().unwrap().len(), 1);
        assert!(!invalid.exists());
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
            serde_json::from_slice::<RecoveryIndexV2>(&fs::read(&index_path).unwrap()).unwrap();
        index.records[0].entry.created_at =
            (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        store.persist_index(&index).unwrap();

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
            serde_json::from_slice::<RecoveryIndexV2>(&fs::read(&index_path).unwrap()).unwrap();
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
        assert!(serde_json::from_slice::<RecoveryIndexV2>(&fs::read(index_path).unwrap()).is_ok());
    }

    #[test]
    fn parseable_index_with_tampered_quota_metadata_is_rebuilt() {
        let temp = tempfile::tempdir().unwrap();
        let store = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let entry = store
            .checkpoint(CheckpointRequest {
                document_id: "tampered-index-document".into(),
                path: None,
                title: "Tampered index".into(),
                content: "recoverable content".into(),
                kind: Some("history".into()),
            })
            .unwrap()
            .unwrap();
        let index_path = temp.path().join(RECOVERY_INDEX_FILE);
        let mut tampered =
            serde_json::from_slice::<RecoveryIndexV2>(&fs::read(&index_path).unwrap()).unwrap();
        let actual_uncompressed = tampered.records[0].uncompressed_bytes;
        tampered.records[0].uncompressed_bytes += 1;
        fs::write(&index_path, serde_json::to_vec(&tampered).unwrap()).unwrap();

        let reopened = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        assert_eq!(reopened.list().unwrap()[0].id, entry.id);

        let rebuilt =
            serde_json::from_slice::<RecoveryIndexV2>(&fs::read(index_path).unwrap()).unwrap();
        assert!(recovery_index_is_complete(&rebuilt));
        assert_eq!(rebuilt.records[0].uncompressed_bytes, actual_uncompressed);
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
            serde_json::from_slice::<RecoveryIndexV2>(&fs::read(&index_path).unwrap()).unwrap();
        incomplete.records.clear();
        fs::write(&index_path, serde_json::to_vec(&incomplete).unwrap()).unwrap();

        let reopened = RecoveryStore::new(temp.path().to_path_buf()).unwrap();
        let listed = reopened.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, entry.id);

        let rebuilt =
            serde_json::from_slice::<RecoveryIndexV2>(&fs::read(index_path).unwrap()).unwrap();
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
        let mut index = serde_json::from_slice::<RecoveryIndexV2>(
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
