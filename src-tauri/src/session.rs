use std::{collections::HashSet, fs, path::PathBuf};

use parking_lot::{Mutex, RwLock};

use crate::{
    error::{ApiError, ApiResult},
    fileio::atomic_write,
    model::SessionV1,
};

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
        let value = normalize(value);
        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|error| ApiError::new("session_error", error.to_string()))?;
        atomic_write(&self.path, &bytes)?;
        *self.value.write() = value.clone();
        *self.load_warning.lock() = None;
        Ok(value)
    }
}

fn normalize(mut value: SessionV1) -> SessionV1 {
    value.schema_version = 1;
    value.workspace_root = value.workspace_root.filter(|path| !path.trim().is_empty());
    let mut seen = HashSet::new();
    value.tabs.retain(|tab| {
        !tab.path.trim().is_empty() && seen.insert(tab.path.replace('/', "\\").to_lowercase())
    });
    value.tabs.truncate(50);
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
}
