use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

use crate::error::{ApiError, ApiResult};

/// An inter-process lock backed by a file handle. On Windows, denying every
/// share mode makes acquisition atomic and releases the lock when the process
/// exits, including abnormal termination.
pub struct DataLock {
    _file: File,
}

/// Serializes InkFlow operations that can invalidate an already-resolved
/// workspace path. The lock lives outside every workspace so using it never
/// leaves application metadata beside the user's documents.
pub struct PathMutationLock {
    _lock: DataLock,
}

pub fn lock_path_mutations() -> ApiResult<PathMutationLock> {
    Ok(PathMutationLock {
        _lock: DataLock::acquire(&path_mutation_lock_path())?,
    })
}

fn path_mutation_lock_path() -> PathBuf {
    std::env::temp_dir()
        .join("InkFlow")
        .join("Locks")
        .join("workspace-path-mutations.lock")
}

impl DataLock {
    pub fn acquire(path: &Path) -> ApiResult<Self> {
        prepare_lock_parent(path)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match open_exclusive(path) {
                Ok(file) => return Ok(Self { _file: file }),
                Err(error) if is_lock_contention(&error) && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) if is_lock_contention(&error) => {
                    return Err(ApiError::new(
                        "data_busy",
                        format!("Another InkFlow process is using this data: {error}"),
                    ));
                }
                Err(error) => {
                    return Err(ApiError::io(
                        "Unable to acquire the InkFlow data lock",
                        error,
                    ));
                }
            }
        }
    }

    /// Attempts to acquire the lock once without waiting for another process.
    ///
    /// Best-effort maintenance work can use this path so lock contention does
    /// not delay a user-visible operation. Permanent I/O failures are still
    /// reported to the caller instead of being mistaken for contention.
    pub fn try_acquire(path: &Path) -> ApiResult<Option<Self>> {
        prepare_lock_parent(path)?;
        match open_exclusive(path) {
            Ok(file) => Ok(Some(Self { _file: file })),
            Err(error) if is_lock_contention(&error) => Ok(None),
            Err(error) => Err(ApiError::io(
                "Unable to acquire the InkFlow data lock",
                error,
            )),
        }
    }
}

fn prepare_lock_parent(path: &Path) -> ApiResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ApiError::io("Unable to create the lock directory", error))?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn is_lock_contention(error: &std::io::Error) -> bool {
    // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION are the only failures
    // produced by a healthy lock file that another process currently owns.
    matches!(error.raw_os_error(), Some(32 | 33))
}

#[cfg(not(target_os = "windows"))]
fn is_lock_contention(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
}

#[cfg(target_os = "windows")]
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(path)
}

#[cfg(not(target_os = "windows"))]
fn open_exclusive(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_releases_the_lock_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.lock");
        let lock = DataLock::acquire(&path).unwrap();
        assert!(path.exists());
        drop(lock);
        assert!(DataLock::acquire(&path).is_ok());
    }

    #[test]
    fn permanent_lock_errors_fail_without_waiting_for_the_busy_timeout() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.lock");
        std::fs::create_dir(&path).unwrap();
        let started = Instant::now();

        let error = DataLock::acquire(&path)
            .err()
            .expect("directory is not a lock file");

        assert_ne!(error.code, "data_busy");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn path_mutation_lock_is_shared_by_all_callers() {
        let first = lock_path_mutations().unwrap();

        let contender = DataLock::try_acquire(&path_mutation_lock_path()).unwrap();

        assert!(contender.is_none());
        drop(first);
        // Other parallel tests may legitimately acquire the process-wide lock
        // as soon as `first` is released. A waiting acquisition proves this
        // caller released it without assuming the lock remains idle.
        assert!(DataLock::acquire(&path_mutation_lock_path()).is_ok());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn try_acquire_reports_contention_without_waiting() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("state.lock");
        let lock = DataLock::acquire(&path).unwrap();
        let started = Instant::now();

        let contender = DataLock::try_acquire(&path).unwrap();

        assert!(contender.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(lock);
    }
}
