use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    RENDERER_PROTOCOL,
    error::{ApiError, ApiResult},
    export,
    fileio::{atomic_write, is_symbolic_link_or_junction},
    model::DiskRevision,
    settings::SettingsStore,
};

use super::service::{CliContext, DestinationSnapshot};

const RENDERER_DIRECTORY_PREFIX: &str = "cli-render-";
const RENDERER_DIRECTORY_NAME_LENGTH: usize = RENDERER_DIRECTORY_PREFIX.len() + 32;
const STALE_RENDERER_DIRECTORY_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub struct RendererRequest<'a> {
    pub operation: &'a str,
    pub title: &'a str,
    pub markdown: &'a str,
    pub document_path: Option<&'a str>,
    pub output_path: Option<&'a str>,
    pub output_destination: Option<&'a DestinationSnapshot>,
    pub expected_output_revision: Option<&'a DiskRevision>,
    pub output_must_not_exist: bool,
    pub output_must_exist: bool,
    pub allow_remote_images: bool,
    pub page_size: Option<&'a str>,
    pub landscape: Option<bool>,
}

struct PrivateRendererDirectory {
    parent: PathBuf,
    path: PathBuf,
}

impl PrivateRendererDirectory {
    fn create(parent: &Path) -> ApiResult<Self> {
        let path = parent.join(format!(
            "{RENDERER_DIRECTORY_PREFIX}{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&path).map_err(|error| {
            ApiError::io("Unable to create the private renderer directory", error)
        })?;
        Ok(Self {
            parent: parent.to_path_buf(),
            path,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateRendererDirectory {
    fn drop(&mut self) {
        cleanup_private_directory(&self.parent, &self.path);
    }
}

pub fn execute(context: &CliContext, request: RendererRequest<'_>) -> ApiResult<Value> {
    let executable = desktop_executable()?;
    let token = Uuid::new_v4().simple().to_string();
    let parent = std::env::temp_dir().join("InkFlow");
    fs::create_dir_all(&parent)
        .map_err(|error| ApiError::io("Unable to create the renderer temp root", error))?;
    cleanup_stale_private_directories(&parent);
    let directory = PrivateRendererDirectory::create(&parent)?;
    let request_path = directory.path().join("request.json");
    let response_path = directory.path().join("response.json");
    let diagnostic_path = directory.path().join("renderer.log");
    let temporary_output_path = match request.operation {
        "html" => Some(directory.path().join("render-output.tmp.html")),
        "pdf" => Some(directory.path().join("render-output.tmp.pdf")),
        _ => None,
    };
    let temporary_output_string = temporary_output_path
        .as_deref()
        .map(|path| path.to_string_lossy().into_owned());
    let settings = SettingsStore::load(context.data_dir.join("settings.json")).get();
    let payload = json!({
        "protocol": RENDERER_PROTOCOL,
        "token": token,
        "operation": request.operation,
        "title": request.title,
        "markdown": request.markdown,
        "documentPath": request.document_path,
        "workspaceRoot": context.root.as_ref().map(|path| path.to_string_lossy().into_owned()),
        "temporaryOutputPath": temporary_output_string,
        "allowRemoteImages": request.allow_remote_images,
        "editorFont": settings.editor_font,
        "pageSize": request.page_size,
        "landscape": request.landscape,
    });
    let bytes = serde_json::to_vec(&payload)
        .map_err(|error| ApiError::new("renderer_request_error", error.to_string()))?;
    atomic_write(&request_path, &bytes)?;
    let request_path = dunce::canonicalize(&request_path)
        .map_err(|error| ApiError::io("Unable to resolve the renderer request", error))?;

    run_worker(
        &executable,
        &request_path,
        &response_path,
        &diagnostic_path,
        &token,
        if request.operation == "pdf" {
            Duration::from_secs(75)
        } else {
            Duration::from_secs(30)
        },
    )?;
    let data = read_response(&response_path)?;
    complete_request(context, &request, temporary_output_path.as_deref(), data)
}

fn run_worker(
    executable: &Path,
    request_path: &Path,
    response_path: &Path,
    diagnostic_path: &Path,
    token: &str,
    timeout: Duration,
) -> ApiResult<()> {
    let mut command = Command::new(executable);
    command
        .arg("--inkflow-render-worker")
        .arg(request_path)
        .arg(response_path)
        .arg(token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| ApiError::io("Unable to start the InkFlow renderer", error))?;
    let deadline = Instant::now() + timeout;
    loop {
        if response_path.is_file() {
            terminate_child(&mut child);
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| ApiError::io("Unable to inspect the InkFlow renderer", error))?
        {
            if response_path.is_file() {
                return Ok(());
            }
            return Err(ApiError::new(
                "renderer_process_error",
                renderer_diagnostic_message(
                    &format!(
                        "The InkFlow renderer exited with {status} without a complete response."
                    ),
                    diagnostic_path,
                ),
            ));
        }
        let stop_error = if super::cancelled() {
            Some(ApiError::new(
                "cancelled",
                "The rendering operation was cancelled before its private output was committed.",
            ))
        } else if Instant::now() >= deadline {
            Some(ApiError::new(
                "renderer_timeout",
                renderer_diagnostic_message(
                    &format!(
                        "The InkFlow renderer exceeded {} seconds.",
                        timeout.as_secs()
                    ),
                    diagnostic_path,
                ),
            ))
        } else {
            None
        };
        if let Some(error) = stop_error {
            terminate_child(&mut child);
            return if response_path.is_file() {
                Ok(())
            } else {
                Err(error)
            };
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn terminate_child(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn complete_request(
    context: &CliContext,
    request: &RendererRequest<'_>,
    temporary_output: Option<&Path>,
    data: Value,
) -> ApiResult<Value> {
    if request.operation == "fragment" {
        return Ok(data);
    }
    if !matches!(request.operation, "html" | "pdf") {
        return Err(ApiError::new(
            "invalid_renderer_operation",
            "The renderer operation is not supported.",
        ));
    }
    if super::cancelled() {
        return Err(ApiError::new(
            "cancelled",
            "The rendering operation was cancelled before its private output was committed.",
        ));
    }
    if data.get("prepared").and_then(Value::as_bool) != Some(true) {
        return Err(ApiError::new(
            "invalid_renderer_response",
            "The renderer did not confirm a complete private output.",
        ));
    }
    let temporary_output = temporary_output.ok_or_else(|| {
        ApiError::new(
            "invalid_renderer_response",
            "The renderer request has no private output path.",
        )
    })?;
    let output = request.output_path.ok_or_else(|| {
        ApiError::new(
            "missing_output_path",
            "Choose a destination for the exported file.",
        )
    })?;
    let destination = request.output_destination.ok_or_else(|| {
        ApiError::new(
            "invalid_renderer_response",
            "The renderer request has no validated destination snapshot.",
        )
    })?;
    let output_path = context.destination_path(Path::new(output))?;
    if destination.path() != output_path {
        return Err(ApiError::new(
            "path_changed",
            "The renderer destination no longer matches the validated output path.",
        ));
    }
    let bytes = fs::read(temporary_output)
        .map_err(|error| ApiError::io("Unable to read the prepared renderer output", error))?;
    let guard = export_guard(request);
    export::write_export_bytes_validated(&output_path, &bytes, guard.as_ref(), || {
        context.revalidate_destination(destination)
    })?;
    let _ = fs::remove_file(temporary_output);
    Ok(json!({
        "outcome": {
            "action": "saved",
            "path": output_path,
        }
    }))
}

fn export_guard(request: &RendererRequest<'_>) -> Option<export::ExportWriteGuard> {
    if request.expected_output_revision.is_none()
        && !request.output_must_not_exist
        && !request.output_must_exist
    {
        None
    } else {
        Some(export::ExportWriteGuard {
            expected_revision: request.expected_output_revision.cloned(),
            create_only: request.output_must_not_exist,
            require_existing: request.output_must_exist,
        })
    }
}

fn renderer_diagnostic_message(message: &str, diagnostic_path: &Path) -> String {
    let diagnostic = fs::read_to_string(diagnostic_path)
        .unwrap_or_default()
        .lines()
        .take(12)
        .collect::<Vec<_>>()
        .join(", ");
    if diagnostic.is_empty() {
        message.to_string()
    } else {
        format!("{message} Stages: {diagnostic}")
    }
}

fn read_response(path: &Path) -> ApiResult<Value> {
    let bytes = fs::read(path)
        .map_err(|error| ApiError::io("Unable to read the renderer response", error))?;
    let response: Value = serde_json::from_slice(&bytes)
        .map_err(|error| ApiError::new("invalid_renderer_response", error.to_string()))?;
    if response.get("protocol").and_then(Value::as_str) != Some(RENDERER_PROTOCOL) {
        return Err(ApiError::new(
            "invalid_renderer_response",
            "The renderer response protocol is invalid.",
        ));
    }
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(response.get("data").cloned().unwrap_or(Value::Null));
    }
    let error = response.get("error").cloned().unwrap_or(Value::Null);
    Err(ApiError::new(
        error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("renderer_error"),
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("The InkFlow renderer failed."),
    ))
}

fn desktop_executable() -> ApiResult<PathBuf> {
    if let Some(value) = std::env::var_os("INKFLOW_DESKTOP_EXE") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(ApiError::new(
            "renderer_unavailable",
            "INKFLOW_DESKTOP_EXE does not identify an InkFlow executable.",
        ));
    }
    let current = std::env::current_exe()
        .map_err(|error| ApiError::io("Unable to locate inkflow-cli.exe", error))?;
    for name in ["InkFlow.exe", "inkflow.exe"] {
        let candidate = current.with_file_name(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    Err(ApiError::new(
        "renderer_unavailable",
        "InkFlow.exe was not found beside inkflow-cli.exe. Set INKFLOW_DESKTOP_EXE during development.",
    ))
}

fn cleanup_private_directory(parent: &Path, directory: &Path) {
    for attempt in 0..4 {
        if !directory.exists() || !is_safe_private_renderer_directory(parent, directory) {
            break;
        }
        if fs::remove_dir_all(directory).is_ok() || !directory.exists() {
            break;
        }
        if attempt < 3 {
            thread::sleep(Duration::from_millis(25));
        }
    }
}

fn cleanup_stale_private_directories(parent: &Path) {
    cleanup_stale_private_directories_at(parent, SystemTime::now());
}

fn cleanup_stale_private_directories_at(parent: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let directory = entry.path();
        if !is_safe_private_renderer_directory(parent, &directory) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };
        if age >= STALE_RENDERER_DIRECTORY_AGE {
            cleanup_private_directory(parent, &directory);
        }
    }
}

fn is_safe_private_renderer_directory(parent: &Path, directory: &Path) -> bool {
    directory.parent() == Some(parent)
        && directory
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(is_private_renderer_directory_name)
        && directory
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
        && is_symbolic_link_or_junction(directory).is_ok_and(|is_link| !is_link)
}

fn is_private_renderer_directory_name(name: &str) -> bool {
    if name.len() != RENDERER_DIRECTORY_NAME_LENGTH {
        return false;
    }
    let Some(identifier) = name.strip_prefix(RENDERER_DIRECTORY_PREFIX) else {
        return false;
    };
    let bytes = identifier.as_bytes();
    bytes
        .iter()
        .all(|value| value.is_ascii_digit() || matches!(value, b'a'..=b'f'))
        && bytes[12] == b'4'
        && matches!(bytes[16], b'8' | b'9' | b'a' | b'b')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_reader_preserves_renderer_errors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("response.json");
        fs::write(
            &path,
            br#"{"protocol":"inkflow.renderer/v3","ok":false,"error":{"code":"render_failed","message":"bad diagram"}}"#,
        )
        .unwrap();
        let error = read_response(&path).unwrap_err();
        assert_eq!(error.code, "render_failed");
        assert_eq!(error.message, "bad diagram");
    }

    #[test]
    fn response_reader_rejects_an_outdated_renderer_protocol() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("response.json");
        fs::write(
            &path,
            br#"{"protocol":"inkflow.renderer/v2","ok":true,"data":{}}"#,
        )
        .unwrap();

        let error = read_response(&path).unwrap_err();

        assert_eq!(error.code, "invalid_renderer_response");
    }

    #[test]
    fn private_directory_cleanup_removes_a_partial_pdf() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("InkFlow");
        fs::create_dir_all(&parent).unwrap();
        let directory;
        {
            let guard = PrivateRendererDirectory::create(&parent).unwrap();
            directory = guard.path().to_path_buf();
            fs::write(directory.join("render-output.tmp.pdf"), b"partial").unwrap();
        }

        assert!(!directory.exists());
    }

    #[test]
    fn stale_private_directory_cleanup_is_age_and_name_scoped() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("InkFlow");
        fs::create_dir_all(&parent).unwrap();
        let guard = PrivateRendererDirectory::create(&parent).unwrap();
        let abandoned = guard.path().to_path_buf();
        fs::write(abandoned.join("request.json"), b"private markdown").unwrap();
        std::mem::forget(guard);
        let unrelated = parent.join("cli-render-not-an-inkflow-uuid");
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join("keep.txt"), b"keep").unwrap();

        let now = SystemTime::now();
        cleanup_stale_private_directories_at(&parent, now);
        assert!(abandoned.exists());

        cleanup_stale_private_directories_at(
            &parent,
            now + STALE_RENDERER_DIRECTORY_AGE + Duration::from_secs(1),
        );

        assert!(!abandoned.exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn direct_cleanup_rejects_similar_directory_names() {
        let temp = tempfile::tempdir().unwrap();
        let parent = temp.path().join("InkFlow");
        fs::create_dir_all(&parent).unwrap();
        let similar = parent.join(format!(
            "{RENDERER_DIRECTORY_PREFIX}{}-backup",
            Uuid::new_v4().simple()
        ));
        fs::create_dir(&similar).unwrap();

        cleanup_private_directory(&parent, &similar);

        assert!(similar.exists());
    }

    #[test]
    fn parent_commits_a_complete_private_renderer_output() {
        let temp = tempfile::tempdir().unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let private_output = temp.path().join("prepared.html");
        let destination = temp.path().join("result.html");
        let destination_snapshot = context.capture_destination(&destination).unwrap();
        fs::write(&private_output, b"prepared").unwrap();
        let destination_string = temp
            .path()
            .join(".")
            .join("result.html")
            .to_string_lossy()
            .into_owned();
        let expected_destination_string =
            destination_snapshot.path().to_string_lossy().into_owned();
        let request = renderer_request(
            "html",
            Some(&destination_string),
            Some(&destination_snapshot),
            None,
            true,
            false,
        );

        let data = complete_request(
            &context,
            &request,
            Some(&private_output),
            json!({ "prepared": true }),
        )
        .unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"prepared");
        assert!(!private_output.exists());
        assert_eq!(
            data.pointer("/outcome/path").and_then(Value::as_str),
            Some(expected_destination_string.as_str())
        );
    }

    #[test]
    fn parent_rechecks_the_destination_revision_before_committing() {
        let temp = tempfile::tempdir().unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let private_output = temp.path().join("prepared.html");
        let destination = temp.path().join("result.html");
        fs::write(&destination, b"original").unwrap();
        let destination_snapshot = context.capture_destination(&destination).unwrap();
        let expected = crate::fileio::revision(&destination).unwrap();
        fs::write(&destination, b"external").unwrap();
        fs::write(&private_output, b"prepared").unwrap();
        let destination_string = temp
            .path()
            .join(".")
            .join("result.html")
            .to_string_lossy()
            .into_owned();
        let request = renderer_request(
            "html",
            Some(&destination_string),
            Some(&destination_snapshot),
            Some(&expected),
            false,
            true,
        );

        let error = complete_request(
            &context,
            &request,
            Some(&private_output),
            json!({ "prepared": true }),
        )
        .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(fs::read(&destination).unwrap(), b"external");
        assert!(private_output.exists());
    }

    #[test]
    fn parent_rejects_a_response_without_a_complete_private_output() {
        let temp = tempfile::tempdir().unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let request = renderer_request("html", Some("result.html"), None, None, true, false);

        let error =
            complete_request(&context, &request, None, json!({ "prepared": false })).unwrap_err();

        assert_eq!(error.code, "invalid_renderer_response");
    }

    #[test]
    fn parent_does_not_recreate_a_destination_directory_removed_during_rendering() {
        let temp = tempfile::tempdir().unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let parent = temp.path().join("exports");
        fs::create_dir(&parent).unwrap();
        let destination = parent.join("result.html");
        let destination_snapshot = context.capture_destination(&destination).unwrap();
        let destination_string = destination.to_string_lossy().into_owned();
        let private_output = temp.path().join("prepared.html");
        fs::write(&private_output, b"prepared").unwrap();
        fs::remove_dir(&parent).unwrap();
        let request = renderer_request(
            "html",
            Some(&destination_string),
            Some(&destination_snapshot),
            None,
            true,
            false,
        );

        let error = complete_request(
            &context,
            &request,
            Some(&private_output),
            json!({ "prepared": true }),
        )
        .unwrap_err();

        assert!(!parent.exists());
        assert!(!destination.exists());
        assert!(private_output.exists());
        assert!(!error.code.is_empty());
    }

    #[test]
    fn parent_rejects_a_replaced_destination_directory_identity() {
        let temp = tempfile::tempdir().unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), None).unwrap();
        let parent = temp.path().join("exports");
        let moved_parent = temp.path().join("moved-exports");
        fs::create_dir(&parent).unwrap();
        let destination = parent.join("result.html");
        let destination_snapshot = context.capture_destination(&destination).unwrap();
        fs::rename(&parent, &moved_parent).unwrap();
        fs::create_dir(&parent).unwrap();
        let private_output = temp.path().join("prepared.html");
        fs::write(&private_output, b"prepared").unwrap();
        let destination_string = destination.to_string_lossy().into_owned();
        let request = renderer_request(
            "html",
            Some(&destination_string),
            Some(&destination_snapshot),
            None,
            true,
            false,
        );

        let error = complete_request(
            &context,
            &request,
            Some(&private_output),
            json!({ "prepared": true }),
        )
        .unwrap_err();

        assert_eq!(error.code, "path_changed");
        assert!(!destination.exists());
        assert!(private_output.exists());
    }

    fn renderer_request<'a>(
        operation: &'a str,
        output_path: Option<&'a str>,
        output_destination: Option<&'a DestinationSnapshot>,
        expected_output_revision: Option<&'a DiskRevision>,
        output_must_not_exist: bool,
        output_must_exist: bool,
    ) -> RendererRequest<'a> {
        RendererRequest {
            operation,
            title: "InkFlow",
            markdown: "# Test",
            document_path: None,
            output_path,
            output_destination,
            expected_output_revision,
            output_must_not_exist,
            output_must_exist,
            allow_remote_images: false,
            page_size: None,
            landscape: None,
        }
    }
}
