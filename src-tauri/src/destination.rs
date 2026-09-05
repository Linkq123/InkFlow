use std::path::{Path, PathBuf};

use crate::{
    error::{ApiError, ApiResult},
    fileio::{self, DirectoryIdentityGuard, FileIdentity, canonical_existing, revision},
    model::DiskRevision,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestinationEntryKind {
    Missing,
    File,
    Directory,
}

#[derive(Debug, Clone)]
pub struct DestinationSnapshot {
    path: PathBuf,
    parent: PathBuf,
    parent_identity: FileIdentity,
    entry_kind: DestinationEntryKind,
}

impl DestinationSnapshot {
    pub fn capture_resolved(path: PathBuf) -> ApiResult<Self> {
        let (path, entry_kind) = resolve_destination(&path)?;
        let parent = path.parent().ok_or_else(|| {
            ApiError::new("invalid_path", "The destination has no parent directory.")
        })?;
        let parent = canonical_existing(parent)?;
        if !parent.is_dir() {
            return Err(ApiError::new(
                "missing_output_directory",
                "The destination directory does not exist.",
            ));
        }
        let parent_identity = fileio::directory_identity(&parent)?;
        Ok(Self {
            path,
            parent,
            parent_identity,
            entry_kind,
        })
    }

    pub fn capture_file(path: &Path) -> ApiResult<(Self, Option<DiskRevision>)> {
        Self::capture_file_resolved(path.to_path_buf())
    }

    pub fn capture_file_resolved(path: PathBuf) -> ApiResult<(Self, Option<DiskRevision>)> {
        let snapshot = Self::capture_resolved(path)?;
        let expected_revision = match snapshot.entry_kind {
            DestinationEntryKind::Missing => None,
            DestinationEntryKind::File => Some(revision(snapshot.path())?),
            DestinationEntryKind::Directory => {
                return Err(ApiError::new(
                    "invalid_output_path",
                    "The export destination must be a file.",
                ));
            }
        };
        Ok((snapshot, expected_revision))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn revalidate(&self) -> ApiResult<DirectoryIdentityGuard> {
        // The path was valid when the token was prepared. Any later failure to
        // resolve it is therefore a destination-state conflict, not a new
        // validation error that should bypass the UI's reselect flow.
        let (path, entry_kind) =
            resolve_destination(&self.path).map_err(|_| destination_path_changed())?;
        if !entry_kind_compatible(self.entry_kind, entry_kind) {
            return Err(destination_path_changed());
        }
        self.revalidate_resolved(&path)
    }

    pub fn revalidate_resolved(&self, path: &Path) -> ApiResult<DirectoryIdentityGuard> {
        let (path, entry_kind) =
            resolve_destination(path).map_err(|_| destination_path_changed())?;
        let parent = path.parent().ok_or_else(|| {
            ApiError::new("invalid_path", "The destination has no parent directory.")
        })?;
        let parent = canonical_existing(parent).map_err(|_| destination_path_changed())?;
        if path != self.path
            || parent != self.parent
            || !entry_kind_compatible(self.entry_kind, entry_kind)
        {
            return Err(destination_path_changed());
        }
        fileio::guard_directory_identity(&parent, self.parent_identity)
    }
}

fn destination_path_changed() -> ApiError {
    ApiError::new(
        "path_changed",
        "The destination path changed before the operation could commit.",
    )
}

fn entry_kind_compatible(captured: DestinationEntryKind, current: DestinationEntryKind) -> bool {
    // File appearance/disappearance is reported by the conditional write as a
    // revision conflict. Directories must remain directories (and missing
    // namespace entries must never turn into directories) because a file write
    // cannot safely arbitrate that type change.
    captured == current
        || matches!(
            (captured, current),
            (DestinationEntryKind::Missing, DestinationEntryKind::File)
                | (DestinationEntryKind::File, DestinationEntryKind::Missing)
        )
}

fn resolve_destination(path: &Path) -> ApiResult<(PathBuf, DestinationEntryKind)> {
    if !path.is_absolute() {
        return Err(ApiError::new(
            "invalid_path",
            "The destination path must be absolute.",
        ));
    }
    if path.exists() {
        let canonical = canonical_existing(path)?;
        let entry_kind = if canonical.is_file() {
            DestinationEntryKind::File
        } else if canonical.is_dir() {
            DestinationEntryKind::Directory
        } else {
            return Err(ApiError::new(
                "invalid_output_path",
                "The destination must be a file or directory.",
            ));
        };
        return Ok((canonical, entry_kind));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ApiError::new("invalid_path", "The destination has no parent directory."))?;
    let parent = canonical_existing(parent)?;
    if !parent.is_dir() {
        return Err(ApiError::new(
            "missing_output_directory",
            "The destination directory does not exist.",
        ));
    }
    Ok((
        parent.join(
            path.file_name().ok_or_else(|| {
                ApiError::new("invalid_path", "The destination has no file name.")
            })?,
        ),
        DestinationEntryKind::Missing,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_snapshot_accepts_and_revalidates_an_existing_directory() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("assets");
        std::fs::create_dir(&directory).unwrap();

        let snapshot = DestinationSnapshot::capture_resolved(directory.clone()).unwrap();

        assert_eq!(snapshot.path(), canonical_existing(&directory).unwrap());
        drop(snapshot.revalidate().unwrap());
    }

    #[test]
    fn file_snapshot_rejects_a_directory_without_restricting_generic_callers() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("workspace");
        std::fs::create_dir(&directory).unwrap();

        let error = DestinationSnapshot::capture_file(&directory).unwrap_err();

        assert_eq!(error.code, "invalid_output_path");
        assert!(DestinationSnapshot::capture_resolved(directory).is_ok());
    }

    #[test]
    fn snapshot_detects_a_target_type_change() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("target");
        let snapshot = DestinationSnapshot::capture_resolved(path.clone()).unwrap();
        std::fs::create_dir(&path).unwrap();

        let error = match snapshot.revalidate() {
            Ok(_) => panic!("target type change should invalidate the snapshot"),
            Err(error) => error,
        };
        assert_eq!(error.code, "path_changed");
    }
}
