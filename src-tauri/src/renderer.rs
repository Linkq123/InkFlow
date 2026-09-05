use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::{AppHandle, Manager, State, WebviewWindow};

use crate::{
    RENDERER_PROTOCOL, asset,
    error::{ApiError, ApiResult},
    export,
    fileio::{atomic_write, canonical_existing},
    model::ExportRequest,
};

const RENDER_WORKER_FLAG: &str = "--inkflow-render-worker";
const RENDER_AWAITING_FRONTEND: u8 = 0;
const RENDER_FINISHING: u8 = 1;
const RENDER_COMPLETED: u8 = 2;

#[derive(Debug, Clone)]
pub struct RendererLaunch {
    request_path: PathBuf,
    response_path: PathBuf,
    token: String,
}

#[derive(Debug, Clone)]
struct ValidatedRendererLaunch {
    request_path: PathBuf,
    response_path: PathBuf,
    token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenderWorkerRequest {
    pub protocol: String,
    pub token: String,
    pub operation: String,
    pub title: String,
    pub markdown: String,
    pub document_path: Option<String>,
    pub workspace_root: Option<String>,
    pub temporary_output_path: Option<String>,
    pub allow_remote_images: bool,
    pub editor_font: String,
    pub page_size: Option<String>,
    pub landscape: Option<bool>,
}

struct RendererState {
    request: RenderWorkerRequest,
    response_path: PathBuf,
    diagnostic_path: PathBuf,
    status: AtomicU8,
}

pub fn launch_from_args() -> Option<RendererLaunch> {
    let args: Vec<String> = std::env::args().collect();
    let index = args.iter().position(|value| value == RENDER_WORKER_FLAG)?;
    let request_path = PathBuf::from(args.get(index + 1)?);
    let response_path = PathBuf::from(args.get(index + 2)?);
    let token = args.get(index + 3)?.clone();
    Some(RendererLaunch {
        request_path,
        response_path,
        token,
    })
}

pub fn run(launch: RendererLaunch) {
    let launch = match validate_launch_paths(launch) {
        Ok(launch) => launch,
        Err(_) => return,
    };
    let response_path = launch.response_path.clone();
    if let Err(error) = run_inner(launch) {
        let _ = write_response(
            &response_path,
            json!({
                "protocol": RENDERER_PROTOCOL,
                "ok": false,
                "error": { "code": error.code, "message": error.message }
            }),
        );
    }
}

fn run_inner(launch: ValidatedRendererLaunch) -> ApiResult<()> {
    let bytes = fs::read(&launch.request_path)
        .map_err(|error| ApiError::io("Unable to read the renderer request", error))?;
    let request: RenderWorkerRequest = serde_json::from_slice(&bytes)
        .map_err(|error| ApiError::new("invalid_renderer_request", error.to_string()))?;
    if request.protocol != RENDERER_PROTOCOL || request.token != launch.token {
        return Err(ApiError::new(
            "invalid_renderer_token",
            "The renderer protocol or token is invalid.",
        ));
    }
    if !matches!(request.operation.as_str(), "fragment" | "html" | "pdf") {
        return Err(ApiError::new(
            "invalid_renderer_operation",
            "The renderer operation is not supported.",
        ));
    }

    let private_directory = canonical_existing(launch.request_path.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_renderer_request",
            "The renderer request has no parent directory.",
        )
    })?)?;
    match (
        request.operation.as_str(),
        request.temporary_output_path.as_deref(),
    ) {
        ("html" | "pdf", Some(temporary)) => {
            let temporary = Path::new(temporary);
            let temporary_parent = temporary.parent().ok_or_else(|| {
                ApiError::new(
                    "invalid_renderer_temporary_path",
                    "The renderer temporary output path has no parent directory.",
                )
            })?;
            if !temporary.is_absolute()
                || temporary.exists()
                || canonical_existing(temporary_parent)? != private_directory
            {
                return Err(ApiError::new(
                    "invalid_renderer_temporary_path",
                    "The renderer temporary output path must be a new file in its private request directory.",
                ));
            }
        }
        ("html" | "pdf", None) => {
            return Err(ApiError::new(
                "invalid_renderer_temporary_path",
                "The renderer export request has no private temporary output path.",
            ));
        }
        (_, Some(_)) => {
            return Err(ApiError::new(
                "invalid_renderer_temporary_path",
                "Only HTML and PDF renderer requests may contain a temporary output path.",
            ));
        }
        _ => {}
    }

    let diagnostic_path = launch
        .request_path
        .parent()
        .ok_or_else(|| {
            ApiError::new(
                "invalid_renderer_request",
                "The renderer request has no parent directory.",
            )
        })?
        .join("renderer.log");
    append_diagnostic(&diagnostic_path, "rust:request-validated");
    let state = RendererState {
        request,
        response_path: launch.response_path.clone(),
        diagnostic_path: diagnostic_path.clone(),
        status: AtomicU8::new(RENDER_AWAITING_FRONTEND),
    };
    let mut context = tauri::generate_context!();
    let renderer_asset = context
        .assets()
        .iter()
        .find(|(key, _)| key.as_ref() == "/renderer.html")
        .map(|(_, bytes)| bytes.len());
    append_diagnostic(
        &diagnostic_path,
        &format!(
            "rust:context:dev={}:renderer-asset={renderer_asset:?}",
            tauri::is_dev()
        ),
    );
    let mut renderer_configured = false;
    for window in &mut context.config_mut().app.windows {
        window.create = window.label == "renderer";
        renderer_configured |= window.create;
    }
    if !renderer_configured {
        return Err(ApiError::new(
            "renderer_start_error",
            "No renderer window is configured.",
        ));
    }
    let frontend_timeout = if state.request.operation == "pdf" {
        Duration::from_secs(70)
    } else {
        Duration::from_secs(28)
    };
    let page_diagnostic = diagnostic_path.clone();
    let result = tauri::Builder::default()
        .manage(state)
        .on_page_load(move |_webview, payload| {
            append_diagnostic(
                &page_diagnostic,
                &format!("webview:{:?}:{}", payload.event(), payload.url()),
            );
        })
        .setup(move |app| {
            append_diagnostic(&diagnostic_path, "rust:setup-started");
            let _window = app.get_webview_window("renderer").ok_or_else(|| {
                ApiError::new(
                    "renderer_start_error",
                    "The preconfigured renderer window was not created.",
                )
            })?;
            // Webview operations are deliberately not invoked from setup: on
            // Windows they synchronously wait for an event loop that has not
            // started yet and can deadlock WebView2 initialization.
            append_diagnostic(&diagnostic_path, "rust:webview-configured");

            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                std::thread::sleep(frontend_timeout);
                let state = app_handle.state::<RendererState>();
                if state
                    .status
                    .compare_exchange(
                        RENDER_AWAITING_FRONTEND,
                        RENDER_COMPLETED,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    append_diagnostic(&state.diagnostic_path, "rust:frontend-timeout");
                    let stages = fs::read_to_string(&state.diagnostic_path)
                        .unwrap_or_default()
                        .lines()
                        .take(16)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _ = write_response(
                        &state.response_path,
                        json!({
                            "protocol": RENDERER_PROTOCOL,
                            "ok": false,
                            "error": {
                                "code": "renderer_frontend_timeout",
                                "message": format!(
                                    "The hidden renderer page did not begin export within {} seconds. Stages: {stages}",
                                    frontend_timeout.as_secs()
                                )
                            }
                        }),
                    );
                    app_handle.exit(1);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            renderer_token,
            renderer_request,
            renderer_load_resource,
            renderer_trace,
            renderer_finish,
            renderer_fail,
        ])
        .run(context);
    result.map_err(|error| ApiError::new("renderer_start_error", error.to_string()))
}

#[tauri::command]
fn renderer_token(state: State<'_, RendererState>) -> String {
    state.request.token.clone()
}

#[tauri::command]
fn renderer_request(
    token: String,
    state: State<'_, RendererState>,
) -> ApiResult<RenderWorkerRequest> {
    verify_token(&token, &state)?;
    Ok(state.request.clone())
}

#[tauri::command]
fn renderer_load_resource(
    token: String,
    resource: String,
    state: State<'_, RendererState>,
) -> ApiResult<String> {
    verify_token(&token, &state)?;
    let document = state.request.document_path.as_deref().ok_or_else(|| {
        ApiError::new(
            "missing_document_path",
            "Local resources require a saved Markdown document.",
        )
    })?;
    let document = canonical_existing(Path::new(document))?;
    let workspace = state
        .request
        .workspace_root
        .as_deref()
        .map(Path::new)
        .map(canonical_existing)
        .transpose()?;
    asset::read_resource(&document, workspace.as_deref(), &resource)
}

#[tauri::command]
fn renderer_trace(token: String, stage: String, state: State<'_, RendererState>) -> ApiResult<()> {
    verify_token(&token, &state)?;
    let stage = stage
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(80)
        .collect::<String>();
    append_diagnostic(&state.diagnostic_path, &format!("frontend:{stage}"));
    Ok(())
}

#[tauri::command]
async fn renderer_finish(
    token: String,
    rendered_html: String,
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, RendererState>,
) -> ApiResult<()> {
    verify_token(&token, &state)?;
    if state
        .status
        .compare_exchange(
            RENDER_AWAITING_FRONTEND,
            RENDER_FINISHING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return Err(ApiError::new(
            "renderer_already_completed",
            "The renderer request already completed.",
        ));
    }
    let request = &state.request;
    let response = match request.operation.as_str() {
        "fragment" => Ok(json!({
            "protocol": RENDERER_PROTOCOL,
            "ok": true,
            "data": { "html": rendered_html }
        })),
        "html" => private_output_path(request).and_then(|temporary| {
            let document = export::standalone_html(&export_request(request, rendered_html));
            atomic_write(temporary, document.as_bytes()).map(|()| {
                json!({
                    "protocol": RENDERER_PROTOCOL,
                    "ok": true,
                    "data": { "prepared": true }
                })
            })
        }),
        "pdf" => {
            let export_request = export_request(request, rendered_html);
            match private_output_path(request) {
                Ok(temporary) => {
                    export::render_pdf_to_temporary(&export_request, window, temporary)
                        .await
                        .map(|()| {
                            json!({
                                "protocol": RENDERER_PROTOCOL,
                                "ok": true,
                                "data": { "prepared": true }
                            })
                        })
                }
                Err(error) => Err(error),
            }
        }
        _ => unreachable!("operation was validated before startup"),
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => json!({
            "protocol": RENDERER_PROTOCOL,
            "ok": false,
            "error": {
                "code": error.code,
                "message": error.message
            }
        }),
    };
    let write_result = write_response(&state.response_path, response);
    state.status.store(RENDER_COMPLETED, Ordering::SeqCst);
    exit_after_response(app);
    write_result
}

fn private_output_path(request: &RenderWorkerRequest) -> ApiResult<&Path> {
    request
        .temporary_output_path
        .as_deref()
        .map(Path::new)
        .ok_or_else(|| {
            ApiError::new(
                "invalid_renderer_temporary_path",
                "The renderer export request has no private temporary output path.",
            )
        })
}

#[tauri::command]
fn renderer_fail(
    token: String,
    code: String,
    message: String,
    app: AppHandle,
    state: State<'_, RendererState>,
) -> ApiResult<()> {
    verify_token(&token, &state)?;
    if state
        .status
        .compare_exchange(
            RENDER_AWAITING_FRONTEND,
            RENDER_COMPLETED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return Ok(());
    }
    let write_result = write_response(
        &state.response_path,
        json!({
            "protocol": RENDERER_PROTOCOL,
            "ok": false,
            "error": { "code": code, "message": message }
        }),
    );
    exit_after_response(app);
    write_result
}

fn export_request(request: &RenderWorkerRequest, rendered_html: String) -> ExportRequest {
    ExportRequest {
        title: request.title.clone(),
        rendered_html,
        output_path: None,
        page_size: request.page_size.clone(),
        landscape: request.landscape,
    }
}

fn verify_token(token: &str, state: &RendererState) -> ApiResult<()> {
    if token == state.request.token {
        Ok(())
    } else {
        Err(ApiError::new(
            "invalid_renderer_token",
            "The renderer token is invalid.",
        ))
    }
}

fn validate_launch_paths(launch: RendererLaunch) -> ApiResult<ValidatedRendererLaunch> {
    if launch.token.len() < 32 {
        return Err(ApiError::new(
            "invalid_renderer_token",
            "The renderer token is too short.",
        ));
    }
    let request = canonical_existing(&launch.request_path)?;
    if !request.is_file() {
        return Err(ApiError::new(
            "invalid_renderer_request",
            "The renderer request path is invalid.",
        ));
    }
    let parent = request.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_renderer_request",
            "The renderer request has no parent.",
        )
    })?;
    let response_parent = launch.response_path.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_renderer_response",
            "The renderer response has no parent.",
        )
    })?;
    if canonical_existing(response_parent)? != parent {
        return Err(ApiError::new(
            "invalid_renderer_response",
            "The renderer response must share the private request directory.",
        ));
    }
    let response_name = launch.response_path.file_name().ok_or_else(|| {
        ApiError::new(
            "invalid_renderer_response",
            "The renderer response has no file name.",
        )
    })?;
    let response_path = parent.join(response_name);
    Ok(ValidatedRendererLaunch {
        request_path: request,
        response_path,
        token: launch.token,
    })
}

fn write_response(path: &Path, value: Value) -> ApiResult<()> {
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| ApiError::new("renderer_response_error", error.to_string()))?;
    atomic_write(path, &bytes)
}

fn append_diagnostic(path: &Path, message: &str) {
    use std::io::Write;

    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{message}");
        let _ = file.flush();
    }
}

fn exit_after_response(app: AppHandle) {
    let app = Arc::new(app);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        app.exit(0);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_request_rejects_a_final_destination_field() {
        let request = json!({
            "protocol": RENDERER_PROTOCOL,
            "token": "a".repeat(32),
            "operation": "html",
            "title": "InkFlow",
            "markdown": "# Test",
            "documentPath": null,
            "workspaceRoot": null,
            "temporaryOutputPath": "C:\\private\\render-output.tmp.html",
            "outputPath": "C:\\Users\\writer\\document.html",
            "allowRemoteImages": false,
            "editorFont": "Segoe UI",
            "pageSize": null,
            "landscape": null
        });

        let error = serde_json::from_value::<RenderWorkerRequest>(request).unwrap_err();

        assert!(error.to_string().contains("outputPath"));
    }

    #[test]
    fn response_path_outside_the_request_directory_is_not_trusted() {
        let request_directory = tempfile::tempdir().unwrap();
        let outside_directory = tempfile::tempdir().unwrap();
        let request_path = request_directory.path().join("request.json");
        let response_path = outside_directory.path().join("existing.txt");
        fs::write(&request_path, b"{}").unwrap();
        fs::write(&response_path, b"keep me").unwrap();

        let error = validate_launch_paths(RendererLaunch {
            request_path,
            response_path: response_path.clone(),
            token: "a".repeat(32),
        })
        .unwrap_err();

        assert_eq!(error.code, "invalid_renderer_response");
        assert_eq!(fs::read(&response_path).unwrap(), b"keep me");
    }
}
