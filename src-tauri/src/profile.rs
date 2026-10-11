use std::{ffi::OsString, io, path::PathBuf};

#[cfg(feature = "desktop")]
use std::path::Path;

use directories::ProjectDirs;

pub(crate) const DATA_ENV: &str = "INKFLOW_DATA_DIR";
pub(crate) const DESKTOP_DATA_FLAG: &str = "--inkflow-data-dir";

fn system_data_directory() -> io::Result<PathBuf> {
    ProjectDirs::from("com", "InkFlow", "InkFlow")
        .map(|project| project.data_local_dir().to_path_buf())
        .ok_or_else(|| io::Error::other("Unable to resolve the InkFlow application data directory"))
}

fn select_data_directory(
    argument: Option<PathBuf>,
    environment: Option<OsString>,
    performance: Option<OsString>,
) -> io::Result<PathBuf> {
    match argument
        .or_else(|| environment.map(PathBuf::from))
        .or_else(|| performance.map(PathBuf::from))
    {
        Some(path) if path.is_absolute() => Ok(path),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The InkFlow data directory must be an absolute path",
        )),
        None => system_data_directory(),
    }
}

pub(crate) fn application_data_directory() -> io::Result<PathBuf> {
    select_data_directory(
        None,
        std::env::var_os(DATA_ENV),
        std::env::var_os(crate::PERFORMANCE_PROFILE_ENV),
    )
}

fn data_directory_argument(
    args: impl IntoIterator<Item = OsString>,
) -> io::Result<Option<PathBuf>> {
    let mut selected = None;
    let mut args = args.into_iter().skip(1);
    while let Some(argument) = args.next() {
        if argument == DESKTOP_DATA_FLAG {
            if selected.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Duplicate InkFlow data directory argument",
                ));
            }
            selected = Some(PathBuf::from(args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Missing InkFlow data directory argument",
                )
            })?));
        }
    }
    Ok(selected)
}

pub(crate) fn desktop_data_directory(
    args: impl IntoIterator<Item = OsString>,
) -> io::Result<PathBuf> {
    let selected = select_data_directory(
        data_directory_argument(args)?,
        std::env::var_os(DATA_ENV),
        std::env::var_os(crate::PERFORMANCE_PROFILE_ENV),
    )?;
    std::fs::create_dir_all(&selected)?;
    dunce::canonicalize(selected)
}

#[cfg(feature = "desktop")]
pub(crate) fn configure_desktop(
    config: &mut tauri::utils::config::Config,
    data: &Path,
) -> io::Result<()> {
    let system = system_data_directory()?;
    let system = dunce::canonicalize(&system).unwrap_or(system);
    if data != system {
        // The single-instance plugin uses the application identifier. Keep
        // custom profiles out of the normal window, including when it is open.
        let profile_id = blake3::hash(data.as_os_str().as_encoded_bytes());
        config.identifier = format!("{}.profile.p{}", config.identifier, profile_id.to_hex());
        // Also isolate plugin state and the WebView data directory, not just
        // settings/session/recovery managed directly by InkFlow.
        config.app.app_directories_override = Some(
            tauri::utils::config::AppDirectoriesOverride::Root(data.to_path_buf()),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_profile_argument_overrides_inherited_directories() {
        let temp = tempfile::tempdir().unwrap();
        let requested = temp.path().join("requested profile");
        let args = [
            OsString::from("InkFlow"),
            OsString::from("note.md"),
            OsString::from(DESKTOP_DATA_FLAG),
            requested.clone().into_os_string(),
        ];
        let selected = select_data_directory(
            data_directory_argument(args.clone()).unwrap(),
            Some(temp.path().join("inherited").into_os_string()),
            Some(temp.path().join("performance").into_os_string()),
        )
        .unwrap();
        assert_eq!(selected, requested);
        assert_eq!(
            desktop_data_directory(args).unwrap(),
            dunce::canonicalize(&requested).unwrap()
        );
        assert_eq!(
            select_data_directory(
                None,
                Some(requested.clone().into_os_string()),
                Some(temp.path().join("performance").into_os_string())
            )
            .unwrap(),
            requested
        );
        assert!(select_data_directory(Some("relative".into()), None, None).is_err());
        assert!(
            data_directory_argument([OsString::from("InkFlow"), DESKTOP_DATA_FLAG.into()]).is_err()
        );
    }

    #[cfg(feature = "desktop")]
    #[test]
    fn custom_profiles_have_separate_single_instance_and_plugin_state() {
        let temp = tempfile::tempdir().unwrap();
        let first = desktop_data_directory([
            OsString::from("InkFlow"),
            DESKTOP_DATA_FLAG.into(),
            temp.path().join("first").into_os_string(),
        ])
        .unwrap();
        let second = desktop_data_directory([
            OsString::from("InkFlow"),
            DESKTOP_DATA_FLAG.into(),
            temp.path().join("second").into_os_string(),
        ])
        .unwrap();
        let configure = |data: &Path| {
            let mut config: tauri::utils::config::Config =
                serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
            configure_desktop(&mut config, data).unwrap();
            config
        };
        let original: tauri::utils::config::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let a = configure(&first);
        let b = configure(&second);
        assert_ne!(a.identifier, original.identifier);
        assert_ne!(a.identifier, b.identifier);
        assert_eq!(a.identifier, configure(&first).identifier);
        assert!(
            matches!(a.app.app_directories_override, Some(tauri::utils::config::AppDirectoriesOverride::Root(root)) if root == first)
        );
    }
}
