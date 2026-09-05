use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

#[cfg(any(feature = "cli", feature = "desktop"))]
use std::fs::File;

#[cfg(not(target_os = "windows"))]
use atomic_write_file::AtomicWriteFile;

#[cfg(target_os = "windows")]
use std::{
    collections::{HashMap, HashSet},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle},
    sync::{
        Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};
#[cfg(target_os = "windows")]
use windows::{
    Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo, GetFileInformationByHandleEx, MoveFileW,
            REPLACE_FILE_FLAGS, ReplaceFileW,
        },
    },
    core::PCWSTR,
};

#[cfg(all(any(feature = "cli", feature = "desktop"), target_os = "windows"))]
use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle};

use crate::{
    error::{ApiError, ApiResult},
    model::DiskRevision,
};

#[derive(Debug, PartialEq, Eq)]
pub enum AtomicWriteOutcome {
    Written,
    Conflict(Option<DiskRevision>),
}

#[cfg(any(feature = "cli", feature = "desktop"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    primary: u64,
    secondary: u64,
}

#[cfg(any(feature = "cli", feature = "desktop"))]
/// Keeps a validated destination directory open while a filesystem mutation is
/// committed. On Windows the handle deliberately denies delete sharing, so the
/// directory cannot be renamed or replaced between the final identity check
/// and the mutation.
pub struct DirectoryIdentityGuard {
    _directory: File,
}

#[cfg(any(feature = "cli", feature = "desktop"))]
pub fn directory_identity(path: &Path) -> ApiResult<FileIdentity> {
    open_directory_for_identity(path, true).map(|(_, identity)| identity)
}

#[cfg(any(feature = "cli", feature = "desktop"))]
pub fn guard_directory_identity(
    path: &Path,
    expected: FileIdentity,
) -> ApiResult<DirectoryIdentityGuard> {
    let (directory, current) = open_directory_for_identity(path, false)?;
    if current != expected {
        return Err(ApiError::new(
            "path_changed",
            "The destination directory changed before the operation could commit.",
        ));
    }
    Ok(DirectoryIdentityGuard {
        _directory: directory,
    })
}

#[cfg(all(any(feature = "cli", feature = "desktop"), target_os = "windows"))]
fn open_directory_for_identity(
    path: &Path,
    allow_delete_sharing: bool,
) -> ApiResult<(File, FileIdentity)> {
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;

    if !path.is_dir() {
        return Err(ApiError::new(
            "path_changed",
            "The destination directory no longer exists.",
        ));
    }
    let share_mode = FILE_SHARE_READ
        | FILE_SHARE_WRITE
        | if allow_delete_sharing {
            FILE_SHARE_DELETE
        } else {
            0
        };
    let directory = OpenOptions::new()
        .access_mode(0)
        .share_mode(share_mode)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|error| ApiError::io("Unable to inspect the destination directory", error))?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(
            HANDLE(directory.as_raw_handle()),
            &mut information as *mut BY_HANDLE_FILE_INFORMATION,
        )
    }
    .map_err(|error| {
        ApiError::new(
            "path_inspection_failed",
            format!("Unable to identify the destination directory: {error}"),
        )
    })?;
    Ok((
        directory,
        FileIdentity {
            primary: information.dwVolumeSerialNumber as u64,
            secondary: ((information.nFileIndexHigh as u64) << 32)
                | information.nFileIndexLow as u64,
        },
    ))
}

#[cfg(all(any(feature = "cli", feature = "desktop"), not(target_os = "windows")))]
fn open_directory_for_identity(
    path: &Path,
    _allow_delete_sharing: bool,
) -> ApiResult<(File, FileIdentity)> {
    use std::os::unix::fs::MetadataExt;

    let directory = File::open(path)
        .map_err(|error| ApiError::io("Unable to inspect the destination directory", error))?;
    let metadata = directory
        .metadata()
        .map_err(|error| ApiError::io("Unable to identify the destination directory", error))?;
    if !metadata.is_dir() {
        return Err(ApiError::new(
            "path_changed",
            "The destination directory no longer exists.",
        ));
    }
    Ok((
        directory,
        FileIdentity {
            primary: metadata.dev(),
            secondary: metadata.ino(),
        },
    ))
}

#[cfg(target_os = "windows")]
const STALE_PREPARED_REPLACEMENT_AGE: Duration = Duration::from_secs(60 * 60);
#[cfg(target_os = "windows")]
const STALE_PREPARED_REPLACEMENT_SCAN_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub fn atomic_write(path: &Path, bytes: &[u8]) -> ApiResult<()> {
    let path = prepare_write_target(path)?;
    atomic_write_unconditional(&path, bytes)
}

pub fn atomic_write_if_revision(
    path: &Path,
    bytes: &[u8],
    expected: Option<&DiskRevision>,
) -> ApiResult<AtomicWriteOutcome> {
    let path = prepare_write_target(path)?;
    #[cfg(target_os = "windows")]
    cleanup_atomic_write_siblings(&path);

    let Some(expected) = expected else {
        atomic_write_unconditional(&path, bytes)?;
        return Ok(AtomicWriteOutcome::Written);
    };

    #[cfg(target_os = "windows")]
    {
        conditional_atomic_write_windows(&path, bytes, expected)
    }
    #[cfg(not(target_os = "windows"))]
    {
        conditional_atomic_write_fallback(&path, bytes, expected)
    }
}

/// Atomically replaces `path` only while a file still exists at that exact
/// destination. Unlike [`atomic_write`], this operation never installs the
/// replacement as a newly created file when the destination is moved or
/// removed while the replacement is being prepared.
pub fn atomic_replace_existing(path: &Path, bytes: &[u8]) -> ApiResult<AtomicWriteOutcome> {
    let path = prepare_write_target(path)?;
    #[cfg(target_os = "windows")]
    cleanup_atomic_write_siblings(&path);

    #[cfg(target_os = "windows")]
    {
        atomic_replace_existing_windows(&path, bytes)
    }
    #[cfg(not(target_os = "windows"))]
    {
        atomic_replace_existing_fallback(&path, bytes)
    }
}

pub fn atomic_create_if_absent(path: &Path, bytes: &[u8]) -> ApiResult<AtomicWriteOutcome> {
    atomic_create_if_absent_with_hook(path, bytes, || Ok(()))
}

fn atomic_create_if_absent_with_hook<F>(
    path: &Path,
    bytes: &[u8],
    before_install: F,
) -> ApiResult<AtomicWriteOutcome>
where
    F: FnOnce() -> ApiResult<()>,
{
    let path = prepare_write_target(path)?;
    #[cfg(target_os = "windows")]
    cleanup_atomic_write_siblings(&path);

    let replacement = PreparedReplacement::new(&path, bytes)?;
    let delays = [50, 150, 450];
    let mut last_error = None;
    let mut before_install = Some(before_install);
    for attempt in 0..=delays.len() {
        if let Some(hook) = before_install.take() {
            hook()?;
        }
        #[cfg(target_os = "windows")]
        let result = move_file_if_absent(replacement.path(), &path);
        #[cfg(not(target_os = "windows"))]
        let result = fs::hard_link(replacement.path(), &path);

        match result {
            Ok(()) => return Ok(AtomicWriteOutcome::Written),
            Err(error) => match revision(&path) {
                Ok(current) => return Ok(AtomicWriteOutcome::Conflict(Some(current))),
                Err(_revision_error) if !path.exists() => {
                    last_error = Some(error);
                    if let Some(delay) = delays.get(attempt) {
                        thread::sleep(Duration::from_millis(*delay));
                    }
                }
                Err(revision_error) => return Err(revision_error),
            },
        }
    }

    Err(ApiError::io(
        "Unable to create the destination file atomically",
        last_error.expect("at least one atomic create attempt ran"),
    ))
}

fn prepare_write_target(path: &Path) -> ApiResult<PathBuf> {
    let path = safe_write_target(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| ApiError::io("Unable to create the destination directory", error))?;
    }
    Ok(path)
}

#[cfg(not(target_os = "windows"))]
fn atomic_write_unconditional(path: &Path, bytes: &[u8]) -> ApiResult<()> {
    let delays = [50, 150, 450];
    let mut last_error = None;
    for attempt in 0..=delays.len() {
        let result = (|| -> std::io::Result<()> {
            let mut file = AtomicWriteFile::open(path)?;
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            file.commit()
        })();
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if let Some(delay) = delays.get(attempt) {
                    thread::sleep(Duration::from_millis(*delay));
                }
            }
        }
    }

    Err(ApiError::io(
        "Unable to replace the destination file atomically",
        last_error.expect("at least one atomic write attempt ran"),
    ))
}

#[cfg(target_os = "windows")]
fn atomic_write_unconditional(path: &Path, bytes: &[u8]) -> ApiResult<()> {
    cleanup_atomic_write_siblings(path);
    let replacement = PreparedReplacement::new(path, bytes)?;
    let delays = [50, 150, 450];
    let mut last_error = None;

    for attempt in 0..=delays.len() {
        let result = if path.exists() {
            let backup = unique_sibling_path(path, "replaced")?;
            match replace_file_with_backup(path, replacement.path(), &backup) {
                Ok(()) => {
                    dispose_verified_file(path, &backup);
                    return Ok(());
                }
                Err(error) if backup.exists() => {
                    return Err(recover_partial_replace(path, &backup, error));
                }
                Err(error) => Err(error),
            }
        } else {
            fs::rename(replacement.path(), path)
        };

        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if let Some(delay) = delays.get(attempt) {
                    thread::sleep(Duration::from_millis(*delay));
                }
            }
        }
    }

    Err(ApiError::io(
        "Unable to replace the destination file atomically",
        last_error.expect("at least one atomic write attempt ran"),
    ))
}

#[cfg(not(target_os = "windows"))]
fn conditional_atomic_write_fallback(
    path: &Path,
    bytes: &[u8],
    expected: &DiskRevision,
) -> ApiResult<AtomicWriteOutcome> {
    let current = match revision(path) {
        Ok(current) => current,
        Err(_error) if !path.exists() => return Ok(AtomicWriteOutcome::Conflict(None)),
        Err(error) => return Err(error),
    };
    if &current != expected {
        return Ok(AtomicWriteOutcome::Conflict(Some(current)));
    }
    atomic_write_unconditional(path, bytes)?;
    Ok(AtomicWriteOutcome::Written)
}

#[cfg(not(target_os = "windows"))]
fn atomic_replace_existing_fallback(path: &Path, bytes: &[u8]) -> ApiResult<AtomicWriteOutcome> {
    match revision(path) {
        Ok(_) => {}
        Err(_error) if !path.exists() => return Ok(AtomicWriteOutcome::Conflict(None)),
        Err(error) => return Err(error),
    }
    atomic_write_unconditional(path, bytes)?;
    Ok(AtomicWriteOutcome::Written)
}

#[cfg(target_os = "windows")]
fn atomic_replace_existing_windows(path: &Path, bytes: &[u8]) -> ApiResult<AtomicWriteOutcome> {
    atomic_replace_existing_windows_with_hook(path, bytes, || Ok(()))
}

#[cfg(target_os = "windows")]
fn atomic_replace_existing_windows_with_hook<F>(
    path: &Path,
    bytes: &[u8],
    before_replace: F,
) -> ApiResult<AtomicWriteOutcome>
where
    F: FnOnce() -> ApiResult<()>,
{
    let replacement = PreparedReplacement::new(path, bytes)?;
    let delays = [50, 150, 450];
    let mut last_error = None;
    let mut before_replace = Some(before_replace);

    for attempt in 0..=delays.len() {
        if let Some(hook) = before_replace.take() {
            hook()?;
        }

        // ReplaceFileW requires an existing destination. Deliberately do not
        // fall back to rename here: rename would recreate a path that another
        // process moved or deleted while the replacement was being prepared.
        let backup = unique_sibling_path(path, "replaced")?;
        match replace_file_with_backup(path, replacement.path(), &backup) {
            Ok(()) => {
                dispose_verified_file(path, &backup);
                return Ok(AtomicWriteOutcome::Written);
            }
            Err(error) => {
                if backup.exists() {
                    return Err(recover_partial_replace(path, &backup, error));
                }
                if !path.exists() {
                    return Ok(AtomicWriteOutcome::Conflict(None));
                }
                last_error = Some(error);
                if let Some(delay) = delays.get(attempt) {
                    thread::sleep(Duration::from_millis(*delay));
                }
            }
        }
    }

    Err(ApiError::io(
        "Unable to replace the existing destination file atomically",
        last_error.expect("at least one existing-file replace attempt ran"),
    ))
}

#[cfg(target_os = "windows")]
fn conditional_atomic_write_windows(
    path: &Path,
    bytes: &[u8],
    expected: &DiskRevision,
) -> ApiResult<AtomicWriteOutcome> {
    conditional_atomic_write_windows_with_hook(path, bytes, expected, || Ok(()))
}

#[cfg(target_os = "windows")]
fn conditional_atomic_write_windows_with_hook<F>(
    path: &Path,
    bytes: &[u8],
    expected: &DiskRevision,
    before_replace: F,
) -> ApiResult<AtomicWriteOutcome>
where
    F: FnOnce() -> ApiResult<()>,
{
    let replacement = PreparedReplacement::new(path, bytes)?;
    let delays = [50, 150, 450];
    let mut last_error = None;
    let mut before_replace = Some(before_replace);

    for attempt in 0..=delays.len() {
        let (modified_ms, size) = match revision_metadata(path) {
            Ok(metadata) => metadata,
            Err(_error) if !path.exists() => return Ok(AtomicWriteOutcome::Conflict(None)),
            Err(error) => return Err(error),
        };
        if modified_ms != expected.modified_ms || size != expected.size {
            let current = match revision(path) {
                Ok(current) => current,
                Err(_error) if !path.exists() => return Ok(AtomicWriteOutcome::Conflict(None)),
                Err(error) => return Err(error),
            };
            return Ok(AtomicWriteOutcome::Conflict(Some(current)));
        }

        if let Some(hook) = before_replace.take() {
            hook()?;
        }

        let backup = unique_sibling_path(path, "replaced")?;
        match replace_file_with_backup(path, replacement.path(), &backup) {
            Ok(()) => {
                let replaced_revision = match revision(&backup) {
                    Ok(revision) => revision,
                    Err(error) => {
                        return Err(match restore_conflicting_version(path, &backup, bytes) {
                            Ok(_) => ApiError::new(
                                "atomic_verification_error",
                                format!(
                                    "Unable to verify the version replaced during the atomic save: {error}. The preserved disk version was restored to {}.",
                                    path.to_string_lossy()
                                ),
                            ),
                            Err(rollback_error) => backup_verification_and_rollback_error(
                                &error,
                                &rollback_error,
                                &backup,
                            ),
                        });
                    }
                };
                if &replaced_revision == expected {
                    dispose_verified_file(path, &backup);
                    return Ok(AtomicWriteOutcome::Written);
                }

                let disk_revision = restore_conflicting_version(path, &backup, bytes)?;
                return Ok(AtomicWriteOutcome::Conflict(Some(disk_revision)));
            }
            Err(error) => {
                if backup.exists() {
                    return Err(recover_partial_replace(path, &backup, error));
                }
                if !path.exists() {
                    return Ok(AtomicWriteOutcome::Conflict(None));
                }
                last_error = Some(error);
                if let Some(delay) = delays.get(attempt) {
                    thread::sleep(Duration::from_millis(*delay));
                }
            }
        }
    }

    Err(ApiError::io(
        "Unable to replace the destination file atomically",
        last_error.expect("at least one conditional atomic write attempt ran"),
    ))
}

struct PreparedReplacement {
    path: PathBuf,
}

impl PreparedReplacement {
    fn new(target: &Path, bytes: &[u8]) -> ApiResult<Self> {
        for _ in 0..16 {
            let path = unique_sibling_path(target, "write")?;
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        return Err(ApiError::io(
                            "Unable to prepare the atomic replacement",
                            error,
                        ));
                    }
                    drop(file);
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(ApiError::io(
                        "Unable to create the atomic replacement",
                        error,
                    ));
                }
            }
        }
        Err(ApiError::new(
            "atomic_write_error",
            "Unable to allocate a unique atomic replacement path.",
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PreparedReplacement {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(target_os = "windows")]
fn restore_conflicting_version(
    target: &Path,
    backup: &Path,
    local_bytes: &[u8],
) -> ApiResult<DiskRevision> {
    let mut desired = backup.to_path_buf();
    let mut expected_current_hash = blake3::hash(local_bytes);

    for _ in 0..8 {
        let desired_bytes = fs::read(&desired).map_err(|error| {
            ApiError::new(
                "atomic_rollback_error",
                format!(
                    "Unable to read the preserved disk version at {}: {error}.",
                    desired.to_string_lossy()
                ),
            )
        })?;
        let desired_hash = blake3::hash(&desired_bytes);
        let displaced = unique_sibling_path(target, "rollback")?;
        if let Err(error) = replace_file_with_backup(target, &desired, &displaced) {
            let recovery = if desired.exists() {
                desired.to_string_lossy().into_owned()
            } else if displaced.exists() {
                displaced.to_string_lossy().into_owned()
            } else {
                "an unknown temporary path".to_string()
            };
            return Err(ApiError::new(
                "atomic_rollback_error",
                format!(
                    "Unable to restore the externally modified file: {error}. The preserved version remains at {recovery}."
                ),
            ));
        }

        let displaced_bytes = fs::read(&displaced).map_err(|error| {
            ApiError::new(
                "atomic_rollback_error",
                format!(
                    "The preserved disk version was restored to {}, but the displaced version at {} could not be verified: {error}.",
                    target.to_string_lossy(),
                    displaced.to_string_lossy()
                ),
            )
        })?;
        if blake3::hash(&displaced_bytes) == expected_current_hash {
            dispose_verified_file(target, &displaced);
            return revision(target);
        }

        expected_current_hash = desired_hash;
        desired = displaced;
    }

    Err(ApiError::new(
        "atomic_rollback_error",
        format!(
            "The destination kept changing while InkFlow restored the conflict. The latest preserved version remains at {}.",
            desired.to_string_lossy()
        ),
    ))
}

#[cfg(target_os = "windows")]
fn backup_verification_and_rollback_error(
    verification_error: &ApiError,
    rollback_error: &ApiError,
    original_backup: &Path,
) -> ApiError {
    ApiError::new(
        "atomic_rollback_error",
        format!(
            "Unable to verify the version replaced during the atomic save: {verification_error}. Restoring it also failed: {rollback_error} The original backup was created at {}.",
            original_backup.to_string_lossy()
        ),
    )
}

#[cfg(target_os = "windows")]
fn recover_partial_replace(target: &Path, backup: &Path, error: std::io::Error) -> ApiError {
    if !target.exists() {
        match fs::rename(backup, target) {
            Ok(()) => {
                return ApiError::io("The atomic replacement failed and was rolled back", error);
            }
            Err(restore_error) => {
                return ApiError::new(
                    "atomic_rollback_error",
                    format!(
                        "The atomic replacement failed ({error}) and its original file could not be restored ({restore_error}). The preserved file remains at {}.",
                        backup.to_string_lossy()
                    ),
                );
            }
        }
    }
    ApiError::new(
        "atomic_replace_error",
        format!(
            "The atomic replacement failed ({error}). A preserved version remains at {}.",
            backup.to_string_lossy()
        ),
    )
}

#[cfg(target_os = "windows")]
fn replace_file_with_backup(
    target: &Path,
    replacement: &Path,
    backup: &Path,
) -> std::io::Result<()> {
    let target = wide_path(target);
    let replacement = wide_path(replacement);
    let backup = wide_path(backup);
    // SAFETY: all three UTF-16 buffers are NUL-terminated and remain alive for the call.
    unsafe {
        ReplaceFileW(
            PCWSTR(target.as_ptr()),
            PCWSTR(replacement.as_ptr()),
            PCWSTR(backup.as_ptr()),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
        .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[cfg(target_os = "windows")]
fn move_file_if_absent(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = wide_path(source);
    let destination = wide_path(destination);
    // SAFETY: both UTF-16 buffers are NUL-terminated and remain alive for the call.
    unsafe {
        MoveFileW(PCWSTR(source.as_ptr()), PCWSTR(destination.as_ptr()))
            .map_err(|error| std::io::Error::other(error.to_string()))
    }
}

#[cfg(target_os = "windows")]
fn wide_path(path: &Path) -> Vec<u16> {
    const BACKSLASH: u16 = b'\\' as u16;
    const QUESTION: u16 = b'?' as u16;
    const DOT: u16 = b'.' as u16;
    const VERBATIM_PREFIX: [u16; 4] = [BACKSLASH, BACKSLASH, QUESTION, BACKSLASH];
    const DEVICE_PREFIX: [u16; 4] = [BACKSLASH, BACKSLASH, DOT, BACKSLASH];
    const UNC_PREFIX: [u16; 8] = [
        BACKSLASH,
        BACKSLASH,
        QUESTION,
        BACKSLASH,
        b'U' as u16,
        b'N' as u16,
        b'C' as u16,
        BACKSLASH,
    ];

    let encoded = path.as_os_str().encode_wide().collect::<Vec<_>>();
    let mut result = if encoded.starts_with(&VERBATIM_PREFIX)
        || encoded.starts_with(&DEVICE_PREFIX)
        || !path.is_absolute()
    {
        encoded
    } else if encoded.starts_with(&[BACKSLASH, BACKSLASH]) {
        UNC_PREFIX
            .into_iter()
            .chain(encoded.into_iter().skip(2))
            .collect()
    } else {
        VERBATIM_PREFIX.into_iter().chain(encoded).collect()
    };
    result.push(0);
    result
}

fn unique_sibling_path(target: &Path, kind: &str) -> ApiResult<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_path",
            "The destination path has no parent directory.",
        )
    })?;
    let target_key = blake3::hash(
        target
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .as_bytes(),
    )
    .to_hex();
    for _ in 0..16 {
        let mut name = OsString::from(".");
        name.push(&target_key[..16]);
        name.push(format!(".inkflow-{kind}-{}", uuid::Uuid::new_v4()));
        let candidate = parent.join(name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ApiError::new(
        "atomic_write_error",
        "Unable to allocate a unique atomic backup path.",
    ))
}

#[cfg(target_os = "windows")]
fn dispose_verified_file(target: &Path, path: &Path) {
    let Ok(cleanup) = unique_sibling_path(target, "cleanup") else {
        remove_file_best_effort(path);
        return;
    };
    match fs::rename(path, &cleanup) {
        Ok(()) => remove_file_best_effort(&cleanup),
        Err(_) => remove_file_best_effort(path),
    }
}

#[cfg(target_os = "windows")]
fn cleanup_atomic_write_siblings(target: &Path) {
    let Some(directory) = target.parent() else {
        return;
    };
    if !atomic_write_sibling_scan_due(directory) {
        return;
    }
    cleanup_atomic_write_siblings_with(directory, |path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
            .is_ok_and(|age| age >= STALE_PREPARED_REPLACEMENT_AGE)
    });
}

#[cfg(target_os = "windows")]
fn atomic_write_sibling_scan_due(directory: &Path) -> bool {
    static LAST_SCANS: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();
    let scans = LAST_SCANS.get_or_init(|| Mutex::new(HashMap::new()));
    atomic_write_sibling_scan_due_at(
        &mut scans
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
        directory,
        Instant::now(),
    )
}

#[cfg(target_os = "windows")]
fn atomic_write_sibling_scan_due_at(
    scans: &mut HashMap<PathBuf, Instant>,
    directory: &Path,
    now: Instant,
) -> bool {
    scans.retain(|_, last_scan| {
        now.saturating_duration_since(*last_scan) < STALE_PREPARED_REPLACEMENT_SCAN_INTERVAL
    });
    if scans.contains_key(directory) {
        return false;
    }
    scans.insert(directory.to_path_buf(), now);
    true
}

#[cfg(target_os = "windows")]
fn cleanup_atomic_write_siblings_with<F>(directory: &Path, is_stale_write: F)
where
    F: Fn(&Path) -> bool,
{
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let disposable_cleanup = is_atomic_write_sibling(name, ".inkflow-cleanup-");
        let stale_write = is_atomic_write_sibling(name, ".inkflow-write-") && is_stale_write(&path);
        if disposable_cleanup || stale_write {
            remove_file_best_effort(&path);
        }
    }
}

#[cfg(target_os = "windows")]
fn is_atomic_write_sibling(name: &str, marker: &str) -> bool {
    let Some((target_key, identifier)) = name.rsplit_once(marker) else {
        return false;
    };
    let Some(hash) = target_key.strip_prefix('.') else {
        return false;
    };
    hash.len() == 16
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        && uuid::Uuid::parse_str(identifier).is_ok()
}

#[cfg(target_os = "windows")]
struct DeferredCleanupQueue {
    paths: Mutex<HashSet<PathBuf>>,
    wake: Condvar,
}

#[cfg(target_os = "windows")]
fn deferred_cleanup_queue() -> &'static DeferredCleanupQueue {
    static QUEUE: OnceLock<DeferredCleanupQueue> = OnceLock::new();
    QUEUE.get_or_init(|| DeferredCleanupQueue {
        paths: Mutex::new(HashSet::new()),
        wake: Condvar::new(),
    })
}

#[cfg(target_os = "windows")]
fn enqueue_deferred_cleanup(path: PathBuf) {
    static WORKER_STARTED: AtomicBool = AtomicBool::new(false);
    let queue = deferred_cleanup_queue();
    queue
        .paths
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(path);
    if !WORKER_STARTED.swap(true, Ordering::AcqRel) {
        thread::spawn(deferred_cleanup_worker);
    }
    queue.wake.notify_one();
}

#[cfg(target_os = "windows")]
fn deferred_cleanup_worker() {
    let queue = deferred_cleanup_queue();
    loop {
        let paths = {
            let mut pending = queue
                .paths
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            while pending.is_empty() {
                pending = queue
                    .wake
                    .wait(pending)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            pending.iter().cloned().collect::<Vec<_>>()
        };

        let removed = paths
            .into_iter()
            .filter(|path| fs::remove_file(path).is_ok() || !path.exists())
            .collect::<Vec<_>>();
        let mut pending = queue
            .paths
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for path in removed {
            pending.remove(&path);
        }
        if !pending.is_empty() {
            let (next, _) = queue
                .wake
                .wait_timeout(pending, Duration::from_secs(5))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(next);
        }
    }
}

#[cfg(target_os = "windows")]
fn remove_file_best_effort(path: &Path) {
    for delay in [0, 25, 75] {
        if delay > 0 {
            thread::sleep(Duration::from_millis(delay));
        }
        if fs::remove_file(path).is_ok() || !path.exists() {
            return;
        }
    }
    enqueue_deferred_cleanup(path.to_path_buf());
}

pub fn revision(path: &Path) -> ApiResult<DiskRevision> {
    let bytes = fs::read(path).map_err(|error| ApiError::io("Unable to read the file", error))?;
    revision_from_bytes(path, &bytes)
}

pub fn revision_metadata(path: &Path) -> ApiResult<(u64, u64)> {
    let metadata =
        fs::metadata(path).map_err(|error| ApiError::io("Unable to inspect the file", error))?;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default();
    Ok((modified_ms, metadata.len()))
}

pub fn revision_from_bytes(path: &Path, bytes: &[u8]) -> ApiResult<DiskRevision> {
    let (modified_ms, size) = revision_metadata(path)?;
    Ok(DiskRevision {
        modified_ms,
        size,
        hash: blake3::hash(bytes).to_hex().to_string(),
    })
}

pub fn canonical_existing(path: &Path) -> ApiResult<PathBuf> {
    dunce::canonicalize(path).map_err(|error| ApiError::io("Unable to resolve the path", error))
}

pub fn ensure_within(root: &Path, candidate: &Path) -> ApiResult<PathBuf> {
    let root = canonical_existing(root)?;
    let resolved = if candidate.exists() {
        canonical_existing(candidate)?
    } else {
        let parent = candidate.parent().ok_or_else(|| {
            ApiError::new(
                "invalid_path",
                "The destination path has no parent directory.",
            )
        })?;
        canonical_existing(parent)?.join(candidate.file_name().ok_or_else(|| {
            ApiError::new("invalid_path", "The destination path has no file name.")
        })?)
    };
    if !resolved.starts_with(&root) {
        return Err(ApiError::new(
            "path_outside_workspace",
            "The path is outside the open workspace.",
        ));
    }
    Ok(resolved)
}

#[cfg(target_os = "windows")]
pub fn is_symbolic_link_or_junction(path: &Path) -> ApiResult<bool> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x7;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

    let metadata = fs::symlink_metadata(path)
        .map_err(|error| ApiError::io("Unable to inspect the scoped path", error))?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(false);
    }

    let handle = OpenOptions::new()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ_WRITE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| ApiError::io("Unable to inspect the reparse point", error))?;
    let mut information = FILE_ATTRIBUTE_TAG_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(handle.as_raw_handle()),
            FileAttributeTagInfo,
            (&mut information as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }
    .map_err(|error| {
        ApiError::new(
            "path_inspection_failed",
            format!("Unable to inspect the reparse point tag: {error}"),
        )
    })?;
    Ok(matches!(
        information.ReparseTag,
        IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK
    ))
}

#[cfg(not(target_os = "windows"))]
pub fn is_symbolic_link_or_junction(path: &Path) -> ApiResult<bool> {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .map_err(|error| ApiError::io("Unable to inspect the scoped path", error))
}

fn safe_write_target(path: &Path) -> ApiResult<PathBuf> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| ApiError::io("Unable to inspect the destination", error))?;
        if metadata.file_type().is_symlink() {
            return dunce::canonicalize(path)
                .map_err(|error| ApiError::io("Unable to resolve the destination link", error));
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_complete_content() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        fs::write(&path, b"old").unwrap();
        atomic_write(&path, b"new content").unwrap();
        assert_eq!(fs::read(path).unwrap(), b"new content");
    }

    #[test]
    fn conditional_atomic_write_preserves_an_external_change() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        fs::write(&path, b"original").unwrap();
        let expected = revision(&path).unwrap();
        fs::write(&path, b"external").unwrap();

        let outcome = atomic_write_if_revision(&path, b"local", Some(&expected)).unwrap();

        assert!(matches!(outcome, AtomicWriteOutcome::Conflict(Some(_))));
        assert_eq!(fs::read(path).unwrap(), b"external");
    }

    #[test]
    fn conditional_create_never_replaces_an_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("created-elsewhere.md");

        let outcome = atomic_create_if_absent_with_hook(&path, b"local", || {
            fs::write(&path, b"external")
                .map_err(|error| ApiError::io("Unable to arrange the create race test", error))
        })
        .unwrap();

        assert!(matches!(outcome, AtomicWriteOutcome::Conflict(Some(_))));
        assert_eq!(fs::read(path).unwrap(), b"external");
    }

    #[test]
    fn replace_existing_refuses_to_create_a_missing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("missing.md");

        let outcome = atomic_replace_existing(&path, b"local").unwrap();

        assert_eq!(outcome, AtomicWriteOutcome::Conflict(None));
        assert!(!path.exists());
    }

    #[test]
    fn replace_existing_replaces_the_current_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("existing.md");
        fs::write(&path, b"external").unwrap();

        let outcome = atomic_replace_existing(&path, b"local").unwrap();

        assert_eq!(outcome, AtomicWriteOutcome::Written);
        assert_eq!(fs::read(path).unwrap(), b"local");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn replace_existing_does_not_recreate_a_destination_moved_during_commit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("raced.md");
        let moved = temp.path().join("moved.md");
        fs::write(&path, b"external").unwrap();

        let outcome = atomic_replace_existing_windows_with_hook(&path, b"local", || {
            fs::rename(&path, &moved)
                .map_err(|error| ApiError::io("Unable to arrange the move race test", error))
        })
        .unwrap();

        assert_eq!(outcome, AtomicWriteOutcome::Conflict(None));
        assert!(!path.exists());
        assert_eq!(fs::read(moved).unwrap(), b"external");
        let leftovers = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".inkflow-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "leftover atomic files: {leftovers:?}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn conditional_commit_restores_a_change_made_in_the_final_replace_window() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("raced.md");
        fs::write(&path, b"original").unwrap();
        let expected = revision(&path).unwrap();

        let outcome =
            conditional_atomic_write_windows_with_hook(&path, b"local", &expected, || {
                fs::write(&path, b"external")
                    .map_err(|error| ApiError::io("Unable to arrange the race test", error))
            })
            .unwrap();

        assert!(matches!(outcome, AtomicWriteOutcome::Conflict(Some(_))));
        assert_eq!(fs::read(&path).unwrap(), b"external");
        let leftovers = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".inkflow-"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "leftover atomic files: {leftovers:?}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn conditional_commit_writes_when_the_replaced_revision_is_still_current() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("current.md");
        fs::write(&path, b"original").unwrap();
        let expected = revision(&path).unwrap();

        let outcome = atomic_write_if_revision(&path, b"local", Some(&expected)).unwrap();

        assert_eq!(outcome, AtomicWriteOutcome::Written);
        assert_eq!(fs::read(path).unwrap(), b"local");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_later_write_cleans_a_verified_backup_left_for_retry() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cleanup.md");
        fs::write(&path, b"original").unwrap();
        let stale = unique_sibling_path(&path, "cleanup").unwrap();
        fs::write(&stale, b"verified old content").unwrap();
        let expected = revision(&path).unwrap();

        let outcome = atomic_write_if_revision(&path, b"new content", Some(&expected)).unwrap();

        assert_eq!(outcome, AtomicWriteOutcome::Written);
        assert_eq!(fs::read(&path).unwrap(), b"new content");
        assert!(!stale.exists());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn directory_cleanup_handles_multiple_targets_without_touching_live_or_foreign_files() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.md");
        let second = temp.path().join("second.md");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let verified = unique_sibling_path(&first, "cleanup").unwrap();
        let stale = unique_sibling_path(&second, "write").unwrap();
        let live = unique_sibling_path(&first, "write").unwrap();
        let foreign = temp
            .path()
            .join(".first.md.inkflow-write-not-an-inkflow-uuid");
        let misleading = temp.path().join(format!(
            ".anything.inkflow-cleanup-{}",
            uuid::Uuid::new_v4()
        ));
        for temporary in [&verified, &stale, &live, &foreign, &misleading] {
            fs::write(temporary, b"temporary").unwrap();
        }

        cleanup_atomic_write_siblings_with(temp.path(), |candidate| candidate == stale);

        assert!(!verified.exists());
        assert!(!stale.exists());
        assert!(live.exists());
        assert!(foreign.exists());
        assert!(misleading.exists());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn atomic_write_sibling_scans_are_throttled_per_directory() {
        let mut scans = HashMap::new();
        let now = Instant::now();
        let first = PathBuf::from("first");
        let second = PathBuf::from("second");
        let third = PathBuf::from("third");

        assert!(atomic_write_sibling_scan_due_at(&mut scans, &first, now));
        assert!(!atomic_write_sibling_scan_due_at(
            &mut scans,
            &first,
            now + Duration::from_secs(30)
        ));
        assert!(atomic_write_sibling_scan_due_at(
            &mut scans,
            &second,
            now + Duration::from_secs(30)
        ));
        assert!(atomic_write_sibling_scan_due_at(
            &mut scans,
            &first,
            now + STALE_PREPARED_REPLACEMENT_SCAN_INTERVAL
        ));
        assert!(atomic_write_sibling_scan_due_at(
            &mut scans,
            &third,
            now + STALE_PREPARED_REPLACEMENT_SCAN_INTERVAL * 2
        ));
        assert_eq!(scans.len(), 1);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn atomic_writes_support_long_target_names() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join(format!("{}.md", "a".repeat(250)));

        let sibling = unique_sibling_path(&target, "replaced").unwrap();
        let name = sibling.file_name().unwrap();

        assert!(name.encode_wide().count() <= 255);
        assert!(is_atomic_write_sibling(
            &name.to_string_lossy(),
            ".inkflow-replaced-"
        ));
        atomic_write(&target, b"unconditional").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"unconditional");
        atomic_write(&target, b"replacement").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"replacement");
        fs::remove_file(&target).unwrap();

        assert_eq!(
            atomic_create_if_absent(&target, b"conditional").unwrap(),
            AtomicWriteOutcome::Written
        );
        assert_eq!(fs::read(target).unwrap(), b"conditional");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn backup_verification_errors_keep_the_rollback_failure_and_backup_path() {
        let verification = ApiError::new("io_error", "backup could not be read");
        let rollback = ApiError::new("atomic_rollback_error", "rollback was denied");
        let backup = Path::new(r"C:\notes\.note.md.inkflow-replaced-test");

        let error = backup_verification_and_rollback_error(&verification, &rollback, backup);

        assert_eq!(error.code, "atomic_rollback_error");
        assert!(error.message.contains("backup could not be read"));
        assert!(error.message.contains("rollback was denied"));
        assert!(error.message.contains(backup.to_string_lossy().as_ref()));
    }

    #[test]
    fn rejects_paths_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let error = ensure_within(root.path(), &outside.path().join("note.md")).unwrap_err();
        assert_eq!(error.code, "path_outside_workspace");
    }
}
