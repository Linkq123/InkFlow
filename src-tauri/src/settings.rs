use std::{fs, path::PathBuf};

use parking_lot::RwLock;

use crate::{
    error::{ApiError, ApiResult},
    fileio::atomic_write,
    model::SettingsV1,
};

pub struct SettingsStore {
    path: PathBuf,
    value: RwLock<SettingsV1>,
}

impl SettingsStore {
    pub fn load(path: PathBuf) -> Self {
        let value = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            value: RwLock::new(value),
        }
    }

    pub fn get(&self) -> SettingsV1 {
        self.value.read().clone()
    }

    pub fn update(&self, mut value: SettingsV1) -> ApiResult<SettingsV1> {
        value.schema_version = 1;
        value.page_width = value.page_width.clamp(560, 1400);
        value.font_size = value.font_size.clamp(12, 32);
        value.line_height = value.line_height.clamp(1.2, 2.4);
        value.autosave_delay_ms = value.autosave_delay_ms.clamp(250, 10_000);
        if !matches!(value.theme.as_str(), "system" | "light" | "dark") {
            return Err(ApiError::new("invalid_settings", "Unknown theme value."));
        }
        value.recent_files.truncate(20);
        value.recent_workspaces.truncate(10);

        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|error| ApiError::new("settings_error", error.to_string()))?;
        atomic_write(&self.path, &bytes)?;
        *self.value.write() = value.clone();
        Ok(value)
    }
}
