use std::{collections::HashSet, fs, path::PathBuf};

use parking_lot::{Mutex, RwLock};

use crate::{
    data_lock::DataLock,
    error::{ApiError, ApiResult},
    fileio::{
        AtomicWriteOutcome, atomic_create_if_absent, atomic_write, atomic_write_if_revision,
        revision_from_bytes,
    },
    model::{DiskRevision, SessionV1},
};

pub(crate) const MAX_SESSION_TABS: usize = 50;

pub struct SessionStore {
    path: PathBuf,
    value: RwLock<SessionV1>,
    load_warning: Mutex<Option<String>>,
}

impl SessionStore {
    pub fn load(path: PathBuf) -> Self {
        let (value, load_warning) = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(value) => (normalize(value), None),
                Err(error) => (
                    SessionV1::default(),
                    Some(format!(
                        "The previous session file was invalid and was skipped: {error}"
                    )),
                ),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                (SessionV1::default(), None)
            }
            Err(error) => (
                SessionV1::default(),
                Some(format!(
                    "The previous session file could not be read and was skipped: {error}"
                )),
            ),
        };
        Self {
            path,
            value: RwLock::new(value),
            load_warning: Mutex::new(load_warning),
        }
    }

    pub fn get(&self) -> ApiResult<SessionV1> {
        if let Some(message) = self.load_warning.lock().take() {
            return Err(ApiError::new("session_load_warning", message));
        }
        Ok(self.value.read().clone())
    }

    pub fn update(&self, value: SessionV1) -> ApiResult<SessionV1> {
        self.update_guarded(value, None, false)
    }

    pub fn update_guarded(
        &self,
        value: SessionV1,
        expected_revision: Option<&DiskRevision>,
        must_not_exist: bool,
    ) -> ApiResult<SessionV1> {
        let _lock = DataLock::acquire(&self.path.with_extension("json.lock"))?;
        self.persist_guarded(value, expected_revision, must_not_exist)
            .map(|(value, _)| value)
    }

    #[cfg(any(feature = "cli", test))]
    pub fn update_guarded_snapshot(
        &self,
        value: SessionV1,
        expected_revision: Option<&DiskRevision>,
        must_not_exist: bool,
    ) -> ApiResult<(SessionV1, DiskRevision)> {
        let _lock = DataLock::acquire(&self.path.with_extension("json.lock"))?;
        self.persist_guarded(value, expected_revision, must_not_exist)
    }

    #[cfg(any(feature = "cli", test))]
    pub fn update_scoped_guarded(
        &self,
        expected_revision: Option<&DiskRevision>,
        must_not_exist: bool,
        update: impl FnOnce(SessionV1) -> SessionV1,
    ) -> ApiResult<(SessionV1, DiskRevision)> {
        let _lock = DataLock::acquire(&self.path.with_extension("json.lock"))?;
        let current = read_session(&self.path)?.0;
        self.persist_guarded(update(current), expected_revision, must_not_exist)
    }

    #[cfg(any(feature = "cli", test))]
    pub fn snapshot(&self) -> ApiResult<(SessionV1, Option<DiskRevision>)> {
        let _lock = DataLock::acquire(&self.path.with_extension("json.lock"))?;
        let (value, revision) = read_session(&self.path)?;
        *self.value.write() = value.clone();
        *self.load_warning.lock() = None;
        Ok((value, revision))
    }

    fn persist_guarded(
        &self,
        value: SessionV1,
        expected_revision: Option<&DiskRevision>,
        must_not_exist: bool,
    ) -> ApiResult<(SessionV1, DiskRevision)> {
        let value = normalize(value);
        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|error| ApiError::new("session_error", error.to_string()))?;
        let outcome = if must_not_exist {
            atomic_create_if_absent(&self.path, &bytes)?
        } else if let Some(expected) = expected_revision {
            atomic_write_if_revision(&self.path, &bytes, Some(expected))?
        } else {
            atomic_write(&self.path, &bytes)?;
            AtomicWriteOutcome::Written
        };
        if let AtomicWriteOutcome::Conflict(current) = outcome {
            return Err(ApiError::new(
                "revision_conflict",
                match current {
                    Some(revision) => format!(
                        "The session changed before it could be written (current hash {}).",
                        revision.hash
                    ),
                    None => "The expected session no longer exists.".into(),
                },
            ));
        }
        *self.value.write() = value.clone();
        *self.load_warning.lock() = None;
        let revision = revision_from_bytes(&self.path, &bytes)?;
        Ok((value, revision))
    }
}

#[cfg(any(feature = "cli", test))]
fn read_session(path: &std::path::Path) -> ApiResult<(SessionV1, Option<DiskRevision>)> {
    match fs::read(path) {
        Ok(bytes) => {
            let value = serde_json::from_slice(&bytes)
                .map(normalize)
                .map_err(|error| {
                    ApiError::new(
                        "session_load_warning",
                        format!("The previous session file was invalid and was skipped: {error}"),
                    )
                })?;
            let revision = revision_from_bytes(path, &bytes)?;
            Ok((value, Some(revision)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok((SessionV1::default(), None))
        }
        Err(error) => Err(ApiError::io("Unable to read the session file", error)),
    }
}

fn normalize(mut value: SessionV1) -> SessionV1 {
    value.schema_version = 1;
    value.workspace_root = value.workspace_root.filter(|path| !path.trim().is_empty());
    let mut seen = HashSet::new();
    value.tabs.retain(|tab| {
        !tab.path.trim().is_empty() && seen.insert(tab.path.replace('/', "\\").to_lowercase())
    });
    value.tabs.truncate(MAX_SESSION_TABS);
    for tab in &mut value.tabs {
        if !matches!(tab.mode.as_str(), "live" | "source" | "preview") {
            tab.mode = "live".into();
        }
    }
    if value.active_path.as_ref().is_some_and(|active| {
        let active = active.replace('/', "\\").to_lowercase();
        !value
            .tabs
            .iter()
            .any(|tab| tab.path.replace('/', "\\").to_lowercase() == active)
    }) {
        value.active_path = value.tabs.first().map(|tab| tab.path.clone());
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fileio::revision;
    use crate::model::SessionTabV1;

    #[test]
    fn persists_and_normalizes_the_local_session() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.json");
        let store = SessionStore::load(path.clone());
        let saved = store
            .update(SessionV1 {
                schema_version: 99,
                workspace_root: Some("C:\\notes".into()),
                tabs: vec![SessionTabV1 {
                    path: "C:\\notes\\one.md".into(),
                    mode: "unknown".into(),
                }],
                active_path: Some("C:\\missing.md".into()),
            })
            .unwrap();

        assert_eq!(saved.schema_version, 1);
        assert_eq!(saved.tabs[0].mode, "live");
        assert_eq!(saved.active_path.as_deref(), Some("C:\\notes\\one.md"));
        assert_eq!(SessionStore::load(path).get().unwrap(), saved);
    }

    #[test]
    fn corrupt_session_falls_back_without_blocking_startup() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.json");
        fs::write(&path, b"not-json").unwrap();
        let store = SessionStore::load(path);
        let error = store.get().unwrap_err();
        assert_eq!(error.code, "session_load_warning");
        assert_eq!(store.get().unwrap(), SessionV1::default());
    }

    #[test]
    fn guarded_update_rejects_a_concurrent_change() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.json");
        let store = SessionStore::load(path.clone());
        store.update(SessionV1::default()).unwrap();
        let expected = revision(&path).unwrap();
        fs::write(
            &path,
            br#"{"schemaVersion":1,"workspaceRoot":"C:\\other","tabs":[],"activePath":null}"#,
        )
        .unwrap();

        let error = store
            .update_guarded(SessionV1::default(), Some(&expected), false)
            .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert!(fs::read_to_string(path).unwrap().contains("other"));
    }

    #[test]
    fn scoped_update_returns_the_revision_of_its_merged_value() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.json");
        let store = SessionStore::load(path.clone());
        let (saved, revision) = store
            .update_scoped_guarded(None, true, |mut current| {
                current.workspace_root = Some("C:\\notes".into());
                current
            })
            .unwrap();

        assert_eq!(saved.workspace_root.as_deref(), Some("C:\\notes"));
        assert_eq!(revision, crate::fileio::revision(&path).unwrap());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn snapshot_returns_content_and_revision_from_one_locked_read() {
        use std::thread;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.json");
        let writer_path = path.clone();
        let writer = thread::spawn(move || {
            let store = SessionStore::load(writer_path);
            for index in 0..50 {
                store
                    .update_guarded_snapshot(
                        SessionV1 {
                            schema_version: 1,
                            workspace_root: Some(format!("C:\\notes\\{index}")),
                            tabs: Vec::new(),
                            active_path: None,
                        },
                        None,
                        false,
                    )
                    .unwrap();
            }
        });
        let reader = SessionStore::load(path);
        for _ in 0..50 {
            let (session, revision) = reader.snapshot().unwrap();
            let Some(revision) = revision else { continue };
            let bytes = serde_json::to_vec_pretty(&session).unwrap();
            assert_eq!(revision.hash, blake3::hash(&bytes).to_hex().to_string());
        }
        writer.join().unwrap();
    }
}
