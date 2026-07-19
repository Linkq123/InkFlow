use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use atomic_write_file::AtomicWriteFile;

use crate::{
    error::{ApiError, ApiResult},
    model::DiskRevision,
};

pub fn atomic_write(path: &Path, bytes: &[u8]) -> ApiResult<()> {
    let path = safe_write_target(path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| ApiError::io("Unable to create the destination directory", error))?;
    }

    let delays = [50, 150, 450];
    let mut last_error = None;
    for attempt in 0..=delays.len() {
        let result = (|| -> std::io::Result<()> {
            let mut file = AtomicWriteFile::open(&path)?;
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            file.commit()?;
            Ok(())
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

pub fn revision(path: &Path) -> ApiResult<DiskRevision> {
    let bytes = fs::read(path).map_err(|error| ApiError::io("Unable to read the file", error))?;
    revision_from_bytes(path, &bytes)
}

pub fn revision_from_bytes(path: &Path, bytes: &[u8]) -> ApiResult<DiskRevision> {
    let metadata =
        fs::metadata(path).map_err(|error| ApiError::io("Unable to inspect the file", error))?;
    let modified_ms = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default();
    Ok(DiskRevision {
        modified_ms,
        size: metadata.len(),
        hash: blake3::hash(bytes).to_hex().to_string(),
    })
}

pub fn canonical_existing(path: &Path) -> ApiResult<PathBuf> {
    path.canonicalize()
        .map_err(|error| ApiError::io("Unable to resolve the path", error))
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

fn safe_write_target(path: &Path) -> ApiResult<PathBuf> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| ApiError::io("Unable to inspect the destination", error))?;
        if metadata.file_type().is_symlink() {
            return path
                .canonicalize()
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
    fn rejects_paths_outside_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let error = ensure_within(root.path(), &outside.path().join("note.md")).unwrap_err();
        assert_eq!(error.code, "path_outside_workspace");
    }
}
