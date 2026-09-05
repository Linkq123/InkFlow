use std::{
    fs,
    path::{Path, PathBuf},
};

use parking_lot::RwLock;

use crate::{
    data_lock::DataLock,
    error::{ApiError, ApiResult},
    fileio::atomic_write,
    model::SettingsV1,
};

const MAX_FONT_FAMILY_CHARACTERS: usize = 256;

pub struct SettingsStore {
    path: PathBuf,
    value: RwLock<SettingsV1>,
}

impl SettingsStore {
    pub fn load(path: PathBuf) -> Self {
        let value = read_settings(&path).ok().flatten().unwrap_or_default();
        Self {
            path,
            value: RwLock::new(value),
        }
    }

    pub fn get(&self) -> SettingsV1 {
        self.value.read().clone()
    }

    #[cfg(any(feature = "cli", test))]
    pub fn snapshot(&self) -> ApiResult<SettingsV1> {
        let _lock = DataLock::acquire(&self.path.with_extension("json.lock"))?;
        read_settings(&self.path).map(|settings| settings.unwrap_or_default())
    }

    pub fn update(&self, mut value: SettingsV1) -> ApiResult<SettingsV1> {
        let _lock = DataLock::acquire(&self.path.with_extension("json.lock"))?;
        let baseline = self.value.read().clone();
        let latest = read_settings(&self.path)?.unwrap_or_else(|| baseline.clone());
        value = merge_changed_settings(&baseline, value, latest);
        self.persist(value)
    }

    #[cfg(any(feature = "cli", test))]
    pub fn update_latest(&self, update: impl FnOnce(&mut SettingsV1)) -> ApiResult<SettingsV1> {
        let _lock = DataLock::acquire(&self.path.with_extension("json.lock"))?;
        let mut latest = read_settings(&self.path)?.unwrap_or_else(|| self.value.read().clone());
        update(&mut latest);
        self.persist(latest)
    }

    #[cfg(any(feature = "cli", test))]
    pub fn reset(&self) -> ApiResult<SettingsV1> {
        let _lock = DataLock::acquire(&self.path.with_extension("json.lock"))?;
        self.persist(SettingsV1::default())
    }

    fn persist(&self, mut value: SettingsV1) -> ApiResult<SettingsV1> {
        value.schema_version = 1;
        value.page_width = value.page_width.clamp(560, 1400);
        value.font_size = value.font_size.clamp(12, 32);
        value.line_height = value.line_height.clamp(1.2, 2.4);
        value.autosave_delay_ms = value.autosave_delay_ms.clamp(250, 10_000);
        if !matches!(value.theme.as_str(), "system" | "light" | "dark") {
            return Err(ApiError::new("invalid_settings", "Unknown theme value."));
        }
        validate_font_settings(&value)?;
        value.recent_files.truncate(20);
        value.recent_workspaces.truncate(10);

        let bytes = serde_json::to_vec_pretty(&value)
            .map_err(|error| ApiError::new("settings_error", error.to_string()))?;
        atomic_write(&self.path, &bytes)?;
        *self.value.write() = value.clone();
        Ok(value)
    }
}

fn read_settings(path: &Path) -> ApiResult<Option<SettingsV1>> {
    match fs::read(path) {
        Ok(bytes) => {
            let settings: SettingsV1 = serde_json::from_slice(&bytes).map_err(|error| {
                ApiError::new(
                    "settings_load_error",
                    format!("The settings file is invalid and was not changed: {error}"),
                )
            })?;
            validate_font_settings(&settings).map_err(|error| {
                ApiError::new(
                    "settings_load_error",
                    format!(
                        "The settings file contains an unsafe font value and was not changed: {}",
                        error.message
                    ),
                )
            })?;
            Ok(Some(settings))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ApiError::io("Unable to read the settings file", error)),
    }
}

fn validate_font_settings(settings: &SettingsV1) -> ApiResult<()> {
    validate_font_family("Editor font", &settings.editor_font)?;
    validate_font_family("Code font", &settings.code_font)
}

fn validate_font_family(label: &str, value: &str) -> ApiResult<()> {
    let normalized = value.to_ascii_lowercase();
    let contains_unsafe_syntax = value.chars().any(|character| {
        character.is_control() || matches!(character, ';' | '{' | '}' | '<' | '>')
    }) || normalized.contains("url(")
        || normalized.contains("@import")
        || normalized.contains("/*")
        || normalized.contains("*/");
    if value.chars().count() > MAX_FONT_FAMILY_CHARACTERS || contains_unsafe_syntax {
        return Err(ApiError::new(
            "invalid_settings",
            format!(
                "{label} must be a font-family list of at most {MAX_FONT_FAMILY_CHARACTERS} characters without CSS declarations or resource functions."
            ),
        ));
    }
    Ok(())
}

fn merge_changed_settings(
    baseline: &SettingsV1,
    requested: SettingsV1,
    mut latest: SettingsV1,
) -> SettingsV1 {
    macro_rules! merge_field {
        ($field:ident) => {
            if requested.$field != baseline.$field {
                latest.$field = requested.$field;
            }
        };
    }
    merge_field!(locale);
    merge_field!(theme);
    merge_field!(page_width);
    merge_field!(font_size);
    merge_field!(line_height);
    merge_field!(editor_font);
    merge_field!(code_font);
    merge_field!(autosave_delay_ms);
    merge_field!(show_file_tree);
    merge_field!(show_outline);
    merge_field!(focus_mode);
    merge_field!(typewriter_mode);
    if requested.recent_files != baseline.recent_files {
        latest.recent_files = merge_recent(requested.recent_files, latest.recent_files, 20);
    }
    if requested.recent_workspaces != baseline.recent_workspaces {
        latest.recent_workspaces =
            merge_recent(requested.recent_workspaces, latest.recent_workspaces, 10);
    }
    latest
}

fn merge_recent(preferred: Vec<String>, current: Vec<String>, limit: usize) -> Vec<String> {
    use std::collections::HashSet;

    let mut seen = HashSet::new();
    preferred
        .into_iter()
        .chain(current)
        .filter(|path| seen.insert(path.replace('/', "\\").to_lowercase()))
        .take(limit)
        .collect()
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    #[test]
    fn merges_fields_changed_by_another_store() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let first = SettingsStore::load(path.clone());
        let second = SettingsStore::load(path);

        let mut from_first = first.get();
        from_first.theme = "dark".into();
        first.update(from_first).unwrap();

        let mut from_second = second.get();
        from_second.font_size = 20;
        let merged = second.update(from_second).unwrap();

        assert_eq!(merged.theme, "dark");
        assert_eq!(merged.font_size, 20);
    }

    #[test]
    fn latest_update_can_explicitly_clear_recent_paths() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let store = SettingsStore::load(path);
        let mut initial = store.get();
        initial.recent_files = vec!["C:\\notes\\one.md".into()];
        store.update(initial).unwrap();

        let updated = store
            .update_latest(|settings| settings.recent_files.clear())
            .unwrap();

        assert!(updated.recent_files.is_empty());
    }

    #[test]
    fn reset_replaces_every_setting_with_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let store = SettingsStore::load(path);
        let mut initial = store.get();
        initial.theme = "dark".into();
        initial.recent_files = vec!["C:\\notes\\one.md".into()];
        store.update(initial).unwrap();

        let reset = store.reset().unwrap();

        assert_eq!(reset.theme, SettingsV1::default().theme);
        assert!(reset.recent_files.is_empty());
    }

    #[test]
    fn rejects_font_values_that_can_escape_a_css_declaration() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let store = SettingsStore::load(path.clone());
        let baseline = store.update(SettingsV1::default()).unwrap();
        let bytes_before = fs::read(&path).unwrap();
        let mut requested = baseline;
        requested.editor_font = "serif;background-image:url(https://example.invalid/leak)".into();

        let error = store.update(requested).unwrap_err();

        assert_eq!(error.code, "invalid_settings");
        assert_eq!(fs::read(path).unwrap(), bytes_before);
    }

    #[test]
    fn accepts_normal_quoted_font_family_lists() {
        let temp = tempfile::tempdir().unwrap();
        let store = SettingsStore::load(temp.path().join("settings.json"));
        let requested = SettingsV1 {
            editor_font: "\"Noto Sans CJK SC\", 'Microsoft YaHei UI', sans-serif".into(),
            ..SettingsV1::default()
        };

        let saved = store.update(requested.clone()).unwrap();

        assert_eq!(saved.editor_font, requested.editor_font);
    }

    #[test]
    fn merged_updates_preserve_an_invalid_settings_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let invalid = br#"{"theme":"dark"#;
        fs::write(&path, invalid).unwrap();
        let store = SettingsStore::load(path.clone());

        let mut requested = store.get();
        requested.font_size = 20;
        let update_error = store.update(requested).unwrap_err();
        let patch_error = store
            .update_latest(|settings| settings.font_size = 21)
            .unwrap_err();

        assert_eq!(update_error.code, "settings_load_error");
        assert_eq!(patch_error.code, "settings_load_error");
        assert_eq!(fs::read(path).unwrap(), invalid);
    }

    #[test]
    fn strict_snapshot_rejects_and_preserves_an_invalid_settings_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let invalid = br#"{"theme":"dark"#;
        fs::write(&path, invalid).unwrap();
        let store = SettingsStore::load(path.clone());

        let error = store.snapshot().unwrap_err();

        assert_eq!(error.code, "settings_load_error");
        assert_eq!(fs::read(path).unwrap(), invalid);
    }

    #[test]
    fn unsafe_font_in_an_existing_file_is_not_loaded_or_rewritten() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        let unsafe_settings = SettingsV1 {
            code_font: "monospace;}body{background:url(https://example.invalid)".into(),
            ..SettingsV1::default()
        };
        let bytes = serde_json::to_vec_pretty(&unsafe_settings).unwrap();
        fs::write(&path, &bytes).unwrap();

        let store = SettingsStore::load(path.clone());
        let error = store.snapshot().unwrap_err();

        assert_eq!(store.get().code_font, SettingsV1::default().code_font);
        assert_eq!(error.code, "settings_load_error");
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
