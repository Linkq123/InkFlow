use std::{path::Path, sync::OnceLock};

#[cfg(feature = "desktop")]
use std::{path::PathBuf, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};

use crate::{
    data_lock::lock_path_mutations,
    error::{ApiError, ApiResult},
    fileio::{
        AtomicWriteOutcome, atomic_create_if_absent, atomic_replace_existing, atomic_write,
        atomic_write_if_revision,
    },
    model::{DiskRevision, ExportOutcome, ExportRequest},
};

#[derive(Debug, Clone, Default)]
pub struct ExportWriteGuard {
    pub expected_revision: Option<DiskRevision>,
    pub create_only: bool,
    pub require_existing: bool,
}

pub fn export_html(request: ExportRequest) -> ApiResult<ExportOutcome> {
    export_html_guarded(request, None)
}

pub fn export_html_guarded(
    request: ExportRequest,
    guard: Option<&ExportWriteGuard>,
) -> ApiResult<ExportOutcome> {
    let output = request.output_path.as_deref().ok_or_else(|| {
        ApiError::new(
            "missing_output_path",
            "Choose a destination for the HTML file.",
        )
    })?;
    let document = standalone_html(&request);
    write_export_bytes(Path::new(output), document.as_bytes(), guard)?;
    Ok(ExportOutcome {
        action: "saved".into(),
        path: Some(output.into()),
    })
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
pub async fn export_pdf(
    request: ExportRequest,
    window: tauri::WebviewWindow,
) -> ApiResult<ExportOutcome> {
    export_pdf_guarded(request, window, None).await
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
pub async fn export_pdf_guarded(
    request: ExportRequest,
    window: tauri::WebviewWindow,
    guard: Option<&ExportWriteGuard>,
) -> ApiResult<ExportOutcome> {
    export_pdf_guarded_with_temporary(request, window, guard, None).await
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
pub async fn export_pdf_guarded_with_temporary(
    request: ExportRequest,
    window: tauri::WebviewWindow,
    guard: Option<&ExportWriteGuard>,
    temporary_override: Option<&Path>,
) -> ApiResult<ExportOutcome> {
    let output_path = pdf_output_path(&request)?;
    let parent = output_path
        .parent()
        .expect("validated PDF output path has a parent");
    let temporary = temporary_override
        .map(Path::to_path_buf)
        .unwrap_or_else(|| parent.join(format!(".inkflow-pdf-{}.tmp.pdf", uuid::Uuid::new_v4())));
    validate_pdf_temporary(&temporary, Some(&output_path))?;
    render_pdf_to_temporary(&request, window, &temporary).await?;
    let commit_temporary = temporary.clone();
    let commit_guard = guard.cloned();
    match tauri::async_runtime::spawn_blocking(move || {
        commit_pdf_temporary(&request, &commit_temporary, commit_guard.as_ref())
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(ApiError::new("pdf_export_error", error.to_string()))
        }
    }
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
pub async fn render_pdf_to_temporary(
    request: &ExportRequest,
    window: tauri::WebviewWindow,
    temporary: &Path,
) -> ApiResult<()> {
    if request.rendered_html.trim().is_empty() {
        return Err(ApiError::new(
            "empty_export",
            "There is no rendered document to print.",
        ));
    }
    validate_pdf_temporary(temporary, None)?;
    let callback_temporary = temporary.to_path_buf();
    let print_path = temporary.to_string_lossy().into_owned();
    let landscape = request.landscape.unwrap_or(false);
    let page_size = request.page_size.as_deref().unwrap_or("A4").to_string();
    let (sender, receiver) = std::sync::mpsc::channel::<Result<(), String>>();

    window
        .with_webview(move |platform| {
            use webview2_com::{
                Microsoft::Web::WebView2::Win32::{
                    COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE,
                    COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT, ICoreWebView2_7,
                    ICoreWebView2Environment6,
                },
                PrintToPdfCompletedHandler,
            };
            use windows::core::{HSTRING, Interface};

            let completion_sender = sender.clone();
            let setup = (|| -> Result<(), String> {
                let controller = platform.controller();
                let core =
                    unsafe { controller.CoreWebView2() }.map_err(|error| error.to_string())?;
                let printable: ICoreWebView2_7 = core.cast().map_err(|error| error.to_string())?;
                let environment: ICoreWebView2Environment6 = platform
                    .environment()
                    .cast()
                    .map_err(|error| error.to_string())?;
                let settings = unsafe { environment.CreatePrintSettings() }
                    .map_err(|error| error.to_string())?;
                let (mut width, mut height) = if page_size.eq_ignore_ascii_case("letter") {
                    (8.5, 11.0)
                } else {
                    (8.27, 11.69)
                };
                if landscape {
                    std::mem::swap(&mut width, &mut height);
                }
                unsafe {
                    settings
                        .SetOrientation(if landscape {
                            COREWEBVIEW2_PRINT_ORIENTATION_LANDSCAPE
                        } else {
                            COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT
                        })
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetPageWidth(width)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetPageHeight(height)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetMarginTop(0.0)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetMarginBottom(0.0)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetMarginLeft(0.0)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetMarginRight(0.0)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetShouldPrintBackgrounds(true)
                        .map_err(|error| error.to_string())?;
                    settings
                        .SetShouldPrintHeaderAndFooter(false)
                        .map_err(|error| error.to_string())?;
                }
                let handler =
                    PrintToPdfCompletedHandler::create(Box::new(move |status, success| {
                        let result = status.map_err(|error| error.to_string()).and_then(|_| {
                            success
                                .then_some(())
                                .ok_or_else(|| "WebView2 did not create the PDF.".into())
                        });
                        send_pdf_completion(&completion_sender, result, &callback_temporary);
                        Ok(())
                    }));
                unsafe {
                    printable
                        .PrintToPdf(&HSTRING::from(print_path), &settings, &handler)
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })();
            if let Err(error) = setup {
                let _ = sender.send(Err(error));
            }
        })
        .map_err(|error| ApiError::new("pdf_export_error", error.to_string()))?;

    let completion = tauri::async_runtime::spawn_blocking(move || {
        receiver.recv_timeout(Duration::from_secs(60))
    })
    .await;

    let completed = match completion {
        Ok(Ok(completed)) => completed,
        Ok(Err(_)) => {
            let _ = std::fs::remove_file(temporary);
            return Err(ApiError::new(
                "pdf_export_timeout",
                "WebView2 PDF export timed out.",
            ));
        }
        Err(error) => {
            let _ = std::fs::remove_file(temporary);
            return Err(ApiError::new("pdf_export_error", error.to_string()));
        }
    };

    if let Err(message) = completed {
        let _ = std::fs::remove_file(temporary);
        return Err(ApiError::new("pdf_export_error", message));
    }
    Ok(())
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
pub fn commit_pdf_temporary(
    request: &ExportRequest,
    temporary: &Path,
    guard: Option<&ExportWriteGuard>,
) -> ApiResult<ExportOutcome> {
    let result = (|| {
        let output_path = pdf_output_path(request)?;
        if temporary == output_path {
            return Err(ApiError::new(
                "invalid_temporary_path",
                "The PDF temporary path must differ from the destination.",
            ));
        }
        let bytes = std::fs::read(temporary)
            .map_err(|error| ApiError::io("Unable to read the generated PDF", error))?;
        write_export_bytes(&output_path, &bytes, guard)?;
        Ok(ExportOutcome {
            action: "saved".into(),
            path: request.output_path.clone(),
        })
    })();
    let _ = std::fs::remove_file(temporary);
    result
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn pdf_output_path(request: &ExportRequest) -> ApiResult<PathBuf> {
    let output = request.output_path.as_deref().ok_or_else(|| {
        ApiError::new(
            "missing_output_path",
            "Choose a destination for the PDF file.",
        )
    })?;
    let output_path = PathBuf::from(output);
    let parent = output_path.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_output_path",
            "The PDF destination has no parent directory.",
        )
    })?;
    if !parent.is_dir() {
        return Err(ApiError::new(
            "missing_output_directory",
            "The PDF destination directory does not exist.",
        ));
    }
    Ok(output_path)
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn validate_pdf_temporary(temporary: &Path, output: Option<&Path>) -> ApiResult<()> {
    if !temporary.is_absolute()
        || !temporary.parent().is_some_and(Path::is_dir)
        || output.is_some_and(|output| temporary == output)
        || temporary.exists()
    {
        return Err(ApiError::new(
            "invalid_temporary_path",
            "The PDF temporary path must be a new absolute file in an existing directory.",
        ));
    }
    Ok(())
}

pub(crate) fn write_export_bytes(
    path: &Path,
    bytes: &[u8],
    guard: Option<&ExportWriteGuard>,
) -> ApiResult<()> {
    write_export_bytes_validated(path, bytes, guard, || Ok(()))
}

pub(crate) fn write_export_bytes_validated<T, F>(
    path: &Path,
    bytes: &[u8],
    guard: Option<&ExportWriteGuard>,
    validate: F,
) -> ApiResult<()>
where
    F: FnOnce() -> ApiResult<T>,
{
    let _path_guard = lock_path_mutations()?;
    let _destination_guard = validate()?;
    let outcome = match guard {
        Some(ExportWriteGuard {
            expected_revision: Some(expected),
            ..
        }) => atomic_write_if_revision(path, bytes, Some(expected))?,
        Some(ExportWriteGuard {
            expected_revision: None,
            create_only: true,
            ..
        }) => atomic_create_if_absent(path, bytes)?,
        Some(ExportWriteGuard {
            require_existing: true,
            ..
        }) => atomic_replace_existing(path, bytes)?,
        _ => {
            atomic_write(path, bytes)?;
            AtomicWriteOutcome::Written
        }
    };
    match outcome {
        AtomicWriteOutcome::Written => Ok(()),
        AtomicWriteOutcome::Conflict(current) => Err(ApiError::new(
            "revision_conflict",
            match current {
                Some(revision) => format!(
                    "The output destination changed before it could be written (current hash {}).",
                    revision.hash
                ),
                None => "The output destination no longer exists.".into(),
            },
        )),
    }
}

#[cfg(feature = "desktop")]
fn send_pdf_completion(
    sender: &std::sync::mpsc::Sender<Result<(), String>>,
    result: Result<(), String>,
    temporary: &Path,
) {
    if sender.send(result).is_err() {
        let _ = std::fs::remove_file(temporary);
    }
}

#[cfg(all(feature = "desktop", not(target_os = "windows")))]
pub async fn export_pdf(
    _request: ExportRequest,
    _window: tauri::WebviewWindow,
) -> ApiResult<ExportOutcome> {
    Err(ApiError::new(
        "unsupported_platform",
        "PDF export is currently available on Windows only.",
    ))
}

pub fn standalone_html(request: &ExportRequest) -> String {
    let title = escape_html(&request.title);
    let katex_css = request
        .rendered_html
        .contains("class=\"katex")
        .then(self_contained_katex_css)
        .unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>{title}</title>
  <style>{}\n{}</style>
</head>
<body><main class="inkflow-document">{}</main></body>
</html>"#,
        export_css(
            request.page_size.as_deref(),
            request.landscape.unwrap_or(false)
        ),
        katex_css,
        request.rendered_html
    )
}

fn self_contained_katex_css() -> &'static str {
    static CSS: OnceLock<String> = OnceLock::new();
    CSS.get_or_init(|| {
        let mut css = include_str!("../../node_modules/katex/dist/katex.min.css").to_string();
        let fonts: &[(&str, &[u8])] = &[
            (
                "KaTeX_AMS-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_AMS-Regular.woff2"),
            ),
            (
                "KaTeX_Caligraphic-Bold",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Caligraphic-Bold.woff2"),
            ),
            (
                "KaTeX_Caligraphic-Regular",
                include_bytes!(
                    "../../node_modules/katex/dist/fonts/KaTeX_Caligraphic-Regular.woff2"
                ),
            ),
            (
                "KaTeX_Fraktur-Bold",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Fraktur-Bold.woff2"),
            ),
            (
                "KaTeX_Fraktur-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Fraktur-Regular.woff2"),
            ),
            (
                "KaTeX_Main-Bold",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Main-Bold.woff2"),
            ),
            (
                "KaTeX_Main-BoldItalic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Main-BoldItalic.woff2"),
            ),
            (
                "KaTeX_Main-Italic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Main-Italic.woff2"),
            ),
            (
                "KaTeX_Main-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Main-Regular.woff2"),
            ),
            (
                "KaTeX_Math-BoldItalic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Math-BoldItalic.woff2"),
            ),
            (
                "KaTeX_Math-Italic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Math-Italic.woff2"),
            ),
            (
                "KaTeX_SansSerif-Bold",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_SansSerif-Bold.woff2"),
            ),
            (
                "KaTeX_SansSerif-Italic",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_SansSerif-Italic.woff2"),
            ),
            (
                "KaTeX_SansSerif-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_SansSerif-Regular.woff2"),
            ),
            (
                "KaTeX_Script-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Script-Regular.woff2"),
            ),
            (
                "KaTeX_Size1-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Size1-Regular.woff2"),
            ),
            (
                "KaTeX_Size2-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Size2-Regular.woff2"),
            ),
            (
                "KaTeX_Size3-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Size3-Regular.woff2"),
            ),
            (
                "KaTeX_Size4-Regular",
                include_bytes!("../../node_modules/katex/dist/fonts/KaTeX_Size4-Regular.woff2"),
            ),
            (
                "KaTeX_Typewriter-Regular",
                include_bytes!(
                    "../../node_modules/katex/dist/fonts/KaTeX_Typewriter-Regular.woff2"
                ),
            ),
        ];
        for (name, bytes) in fonts {
            css = css.replace(
                &format!("fonts/{name}.woff2"),
                &format!("data:font/woff2;base64,{}", STANDARD.encode(bytes)),
            );
        }
        regex::Regex::new(r#",url\(fonts/[^)]*\.(?:woff|ttf)\) format\("[^"]+"\)"#)
            .expect("valid KaTeX fallback font pattern")
            .replace_all(&css, "")
            .into_owned()
    })
}

pub fn export_css(page_size: Option<&str>, landscape: bool) -> String {
    let size = match page_size.unwrap_or("A4").to_ascii_lowercase().as_str() {
        "letter" => "Letter",
        _ => "A4",
    };
    let orientation = if landscape { " landscape" } else { "" };
    format!(
        r#"
:root{{color-scheme:light;--ink:#242424;--muted:#6f6f6f;--line:#dededb}}
*{{box-sizing:border-box}}
body{{margin:0;background:#fff;color:var(--ink);font:16px/1.75 "Segoe UI", "Microsoft YaHei UI", sans-serif}}
.inkflow-document{{max-width:820px;margin:0 auto;padding:56px 44px 96px}}
h1,h2,h3,h4,h5,h6{{line-height:1.28;margin:1.7em 0 .65em;font-weight:650}}
h1{{font-size:2.1em}} h2{{font-size:1.65em;border-bottom:1px solid var(--line);padding-bottom:.25em}}
p,ul,ol,blockquote,pre,table{{margin:1em 0}}
a{{color:#356bc4;text-decoration:none}} a:hover{{text-decoration:underline}}
blockquote{{border-left:3px solid #9b9b96;margin-left:0;padding:.25em 1em;color:var(--muted)}}
code{{font-family:"Cascadia Mono",Consolas,monospace;font-size:.9em;background:#f2f2ef;border-radius:4px;padding:.12em .35em}}
pre{{background:#f6f6f3;border:1px solid #e7e7e3;border-radius:8px;padding:1em;overflow:auto}}
pre code{{background:none;padding:0}}
table{{border-collapse:collapse;width:100%}} th,td{{border:1px solid var(--line);padding:.45em .7em;text-align:left}}
img,svg{{max-width:100%;height:auto}} hr{{border:0;border-top:1px solid var(--line);margin:2em 0}}
.katex-display{{overflow-x:auto;overflow-y:hidden}}
@page{{size:{size}{orientation};margin:18mm 16mm}}
@media print{{.inkflow-document{{max-width:none;margin:0;padding:0}} pre,blockquote,table,img,svg{{break-inside:avoid}} a{{color:inherit}}}}
"#
    )
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_the_export_title() {
        let html = standalone_html(&ExportRequest {
            title: "<unsafe>".into(),
            rendered_html: "<p>safe</p>".into(),
            output_path: None,
            page_size: None,
            landscape: None,
        });
        assert!(html.contains("&lt;unsafe&gt;"));
        assert!(!html.contains("<title><unsafe>"));
        assert!(!html.contains("data:font/woff2;base64,"));
    }

    #[test]
    fn embeds_katex_assets_when_math_is_present() {
        let html = standalone_html(&ExportRequest {
            title: "Math".into(),
            rendered_html: "<span class=\"katex\">formula</span>".into(),
            output_path: None,
            page_size: None,
            landscape: None,
        });

        assert!(html.contains("data:font/woff2;base64,"));
        assert!(!html.contains("url(fonts/"));
    }

    #[test]
    fn guarded_export_refuses_to_replace_a_concurrently_created_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.html");
        std::fs::write(&path, b"external").unwrap();

        let error = write_export_bytes(
            &path,
            b"inkflow",
            Some(&ExportWriteGuard {
                expected_revision: None,
                create_only: true,
                require_existing: false,
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
    }

    #[test]
    fn guarded_export_refuses_a_stale_revision() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.html");
        std::fs::write(&path, b"first").unwrap();
        let expected = crate::fileio::revision(&path).unwrap();
        std::fs::write(&path, b"external").unwrap();

        let error = write_export_bytes(
            &path,
            b"inkflow",
            Some(&ExportWriteGuard {
                expected_revision: Some(expected),
                create_only: false,
                require_existing: true,
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
    }

    #[test]
    fn forced_export_does_not_recreate_a_moved_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.html");
        let moved = temp.path().join("moved.html");
        std::fs::write(&path, b"original").unwrap();
        std::fs::rename(&path, &moved).unwrap();

        let error = write_export_bytes(
            &path,
            b"inkflow",
            Some(&ExportWriteGuard {
                expected_revision: None,
                create_only: false,
                require_existing: true,
            }),
        )
        .unwrap_err();

        assert_eq!(error.code, "revision_conflict");
        assert!(!path.exists());
        assert_eq!(std::fs::read(moved).unwrap(), b"original");
    }

    #[test]
    fn forced_export_still_replaces_an_existing_changed_destination() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("export.html");
        std::fs::write(&path, b"changed externally").unwrap();

        write_export_bytes(
            &path,
            b"inkflow",
            Some(&ExportWriteGuard {
                expected_revision: None,
                create_only: false,
                require_existing: true,
            }),
        )
        .unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"inkflow");
    }

    #[test]
    #[cfg(all(feature = "desktop", target_os = "windows"))]
    fn commits_a_private_pdf_and_removes_the_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("export.pdf");
        let temporary = temp.path().join("private.tmp.pdf");
        std::fs::write(&temporary, b"generated PDF").unwrap();
        let request = ExportRequest {
            title: "PDF".into(),
            rendered_html: "<p>PDF</p>".into(),
            output_path: Some(output.to_string_lossy().into_owned()),
            page_size: Some("A4".into()),
            landscape: Some(false),
        };

        let outcome = commit_pdf_temporary(
            &request,
            &temporary,
            Some(&ExportWriteGuard {
                expected_revision: None,
                create_only: true,
                require_existing: false,
            }),
        )
        .unwrap();

        assert_eq!(outcome.path.as_deref(), request.output_path.as_deref());
        assert_eq!(std::fs::read(output).unwrap(), b"generated PDF");
        assert!(!temporary.exists());
    }

    #[test]
    #[cfg(feature = "desktop")]
    fn late_pdf_completion_removes_the_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("late.pdf");
        std::fs::write(&path, b"late PDF").unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(receiver);

        send_pdf_completion(&sender, Ok(()), &path);

        assert!(!path.exists());
    }
}
