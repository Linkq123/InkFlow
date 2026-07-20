use std::{
    collections::{HashMap, HashSet},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Local;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};
use regex::Regex;

use crate::{
    error::{ApiError, ApiResult},
    fileio::{atomic_write, canonical_existing},
    model::{WriteAssetRequest, WriteAssetResult},
};

const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const MAX_BASE64_IMAGE_BYTES: usize = (MAX_IMAGE_BYTES as usize).div_ceil(3) * 4;

pub fn write_asset(recovery_dir: &Path, request: WriteAssetRequest) -> ApiResult<WriteAssetResult> {
    let (bytes, extension) = asset_bytes_and_extension(&request)?;
    let hash = blake3::hash(&bytes).to_hex().to_string();
    let hash_prefix = &hash[..16];

    let (directory, markdown_prefix, pending) = match request.document_path.as_deref() {
        Some(document_path) => {
            let document = PathBuf::from(document_path);
            let parent = document.parent().ok_or_else(|| {
                ApiError::new("invalid_path", "The document has no parent directory.")
            })?;
            let stem = document
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("document");
            let folder = format!("{stem}.assets");
            (parent.join(&folder), folder, false)
        }
        None => (
            recovery_dir
                .join("assets")
                .join(safe_component(&request.document_id)?),
            "inkflow-asset:".into(),
            true,
        ),
    };
    fs::create_dir_all(&directory)
        .map_err(|error| ApiError::io("Unable to create the asset directory", error))?;

    if let Some(existing) = find_existing_asset(&directory, &hash, hash_prefix, &extension)? {
        return Ok(asset_result(existing, &markdown_prefix, pending));
    }

    let filename = format!(
        "image-{}-{hash_prefix}.{extension}",
        Local::now().format("%Y%m%d-%H%M%S")
    );
    let path = directory.join(filename);
    atomic_write(&path, &bytes)?;
    Ok(asset_result(path, &markdown_prefix, pending))
}

pub fn migrate_pending_assets(
    recovery_dir: &Path,
    document_id: &str,
    document_path: &Path,
    content: &str,
) -> ApiResult<String> {
    let pending = recovery_dir
        .join("assets")
        .join(safe_component(document_id)?);
    if !pending.exists() {
        return Ok(content.to_string());
    }

    let recovery_root = canonical_existing(recovery_dir)?;
    let resolved_pending = canonical_existing(&pending)?;
    if !resolved_pending.starts_with(&recovery_root) {
        return Err(ApiError::new(
            "invalid_asset_path",
            "The pending asset directory is outside the recovery area.",
        ));
    }

    let stem = document_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("document");
    let folder_name = format!("{stem}.assets");
    let destination = document_path
        .parent()
        .ok_or_else(|| ApiError::new("invalid_path", "The document has no parent directory."))?
        .join(&folder_name);
    fs::create_dir_all(&destination)
        .map_err(|error| ApiError::io("Unable to create the document asset directory", error))?;

    let mut replacements = HashMap::new();
    for item in fs::read_dir(&resolved_pending)
        .map_err(|error| ApiError::io("Unable to scan pending assets", error))?
    {
        let source = item
            .map_err(|error| ApiError::io("Unable to inspect a pending asset", error))?
            .path();
        if !source.is_file() {
            continue;
        }
        let filename = source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ApiError::new("invalid_asset_path", "Asset name is not valid Unicode.")
            })?;
        let mut target = destination.join(filename);
        if target.exists() {
            let source_bytes = fs::read(&source)
                .map_err(|error| ApiError::io("Unable to inspect a pending asset", error))?;
            let target_bytes = fs::read(&target)
                .map_err(|error| ApiError::io("Unable to inspect a destination asset", error))?;
            if source_bytes != target_bytes {
                let hash = blake3::hash(&source_bytes).to_hex();
                let stem = source
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("image");
                let extension = source
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("png");
                target = destination.join(format!("{stem}-{}.{extension}", &hash[..16]));
            }
        }
        if !target.exists() {
            fs::copy(&source, &target)
                .map_err(|error| ApiError::io("Unable to migrate a pending asset", error))?;
        }
        let target_filename = target
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                ApiError::new("invalid_asset_path", "Asset name is not valid Unicode.")
            })?;
        replacements.insert(
            format!("inkflow-asset://{filename}"),
            format!("{folder_name}/{target_filename}"),
        );
    }
    Ok(rewrite_image_destinations(content, &replacements))
}

pub fn cleanup_pending_assets(recovery_dir: &Path, document_id: &str) -> ApiResult<()> {
    let pending = recovery_dir
        .join("assets")
        .join(safe_component(document_id)?);
    if !pending.exists() {
        return Ok(());
    }
    let assets_root = recovery_dir.join("assets");
    let resolved_assets = canonical_existing(&assets_root)?;
    let resolved_pending = canonical_existing(&pending)?;
    if resolved_pending == resolved_assets || !resolved_pending.starts_with(&resolved_assets) {
        return Err(ApiError::new(
            "invalid_asset_path",
            "The pending asset directory is outside its document scope.",
        ));
    }
    fs::remove_dir_all(&resolved_pending)
        .map_err(|error| ApiError::io("Unable to clean the pending asset directory", error))
}

pub fn copy_referenced_assets_for_save_as(
    source_document: &Path,
    destination_document: &Path,
    content: &str,
) -> ApiResult<String> {
    let source_parent = source_document.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_path",
            "The source document has no parent directory.",
        )
    })?;
    let source_scope = canonical_existing(source_parent)?;
    let destination_parent = destination_document.parent().ok_or_else(|| {
        ApiError::new(
            "invalid_path",
            "The destination document has no parent directory.",
        )
    })?;
    let destination_stem = destination_document
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("document");
    let asset_folder = format!("{destination_stem}.assets");
    let destination = destination_parent.join(&asset_folder);
    let paths: Vec<String> = collect_image_destinations(content)
        .into_iter()
        .map(|destination| destination.path)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let mut replacements = HashMap::new();
    for markdown_path in paths {
        if markdown_path.starts_with("http://")
            || markdown_path.starts_with("https://")
            || markdown_path.starts_with("data:")
            || markdown_path.starts_with("inkflow-asset://")
        {
            continue;
        }
        let relative = PathBuf::from(markdown_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        let candidate = if relative.is_absolute() {
            relative
        } else {
            source_parent.join(relative)
        };
        let Ok(source) = canonical_existing(&candidate) else {
            continue;
        };
        if !source.starts_with(&source_scope) || !source.is_file() || !is_image_path(&source) {
            continue;
        }
        let bytes = fs::read(&source)
            .map_err(|error| ApiError::io("Unable to read a referenced image", error))?;
        fs::create_dir_all(&destination).map_err(|error| {
            ApiError::io("Unable to create the destination asset directory", error)
        })?;
        let filename = source
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("image.png");
        let mut target = destination.join(filename);
        if target.exists() && fs::read(&target).ok().as_deref() != Some(bytes.as_slice()) {
            let hash = blake3::hash(&bytes).to_hex();
            let stem = source
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("image");
            let extension = source
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("png");
            target = destination.join(format!("{stem}-{}.{extension}", &hash[..8]));
        }
        if !target.exists() {
            atomic_write(&target, &bytes)?;
        }
        let new_path = format!(
            "{asset_folder}/{}",
            target.file_name().unwrap_or_default().to_string_lossy()
        );
        replacements.insert(markdown_path, new_path);
    }
    Ok(rewrite_image_destinations(content, &replacements))
}

pub fn read_resource(
    document_path: &Path,
    workspace_root: Option<&Path>,
    resource: &str,
) -> ApiResult<String> {
    if resource.starts_with("http://") || resource.starts_with("https://") {
        return Err(ApiError::new(
            "remote_resource_blocked",
            "Remote images are blocked until the document is trusted.",
        ));
    }
    let resource_path = PathBuf::from(resource.replace('/', std::path::MAIN_SEPARATOR_STR));
    let candidate = if resource_path.is_absolute() {
        resource_path
    } else {
        document_path
            .parent()
            .ok_or_else(|| ApiError::new("invalid_path", "The document has no parent directory."))?
            .join(resource_path)
    };
    let resolved = canonical_existing(&candidate)?;
    let allowed = if let Some(root) = workspace_root {
        resolved.starts_with(canonical_existing(root)?)
    } else {
        let parent = canonical_existing(document_path.parent().unwrap_or(Path::new(".")))?;
        resolved.starts_with(parent)
    };
    if !allowed || !resolved.is_file() || !is_image_path(&resolved) {
        return Err(ApiError::new(
            "resource_outside_scope",
            "The image is outside the active document scope.",
        ));
    }
    let metadata = fs::metadata(&resolved)
        .map_err(|error| ApiError::io("Unable to inspect the image", error))?;
    if metadata.len() > 50 * 1024 * 1024 {
        return Err(ApiError::new(
            "resource_too_large",
            "Images larger than 50 MB are not loaded inline.",
        ));
    }
    let bytes =
        fs::read(&resolved).map_err(|error| ApiError::io("Unable to read the image", error))?;
    let mime = mime_for_extension(
        resolved
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default(),
    );
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

fn asset_bytes_and_extension(request: &WriteAssetRequest) -> ApiResult<(Vec<u8>, String)> {
    if let Some(source) = request.source_path.as_deref() {
        let path = canonical_existing(Path::new(source))?;
        if !path.is_file() {
            return Err(ApiError::new(
                "invalid_asset",
                "The dropped asset is not a file.",
            ));
        }
        let metadata = fs::metadata(&path)
            .map_err(|error| ApiError::io("Unable to inspect the source image", error))?;
        if metadata.len() > MAX_IMAGE_BYTES {
            return Err(ApiError::new(
                "asset_too_large",
                "Images cannot exceed 50MB.",
            ));
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("png")
            .to_ascii_lowercase();
        if !is_image_path(&path) {
            return Err(ApiError::new(
                "invalid_asset",
                "The dropped file is not a supported image.",
            ));
        }
        let bytes = fs::read(path)
            .map_err(|error| ApiError::io("Unable to read the source image", error))?;
        return Ok((bytes, extension));
    }

    let encoded = request
        .data_base64
        .as_deref()
        .ok_or_else(|| ApiError::new("invalid_asset", "No image data was provided."))?;
    let payload = encoded
        .split_once(',')
        .map(|(_, data)| data)
        .unwrap_or(encoded);
    validate_base64_payload_length(payload.len())?;
    let bytes = STANDARD
        .decode(payload)
        .map_err(|error| ApiError::new("invalid_asset", error.to_string()))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(ApiError::new(
            "asset_too_large",
            "Images cannot exceed 50MB.",
        ));
    }

    let extension = match request.mime_type.as_deref().unwrap_or("image/png") {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => {
            return Err(ApiError::new(
                "invalid_asset",
                "The pasted data is not a supported image.",
            ));
        }
    };
    Ok((bytes, extension.into()))
}

fn validate_base64_payload_length(length: usize) -> ApiResult<()> {
    if length > MAX_BASE64_IMAGE_BYTES {
        return Err(ApiError::new(
            "asset_too_large",
            "Images cannot exceed 50MB.",
        ));
    }
    Ok(())
}

fn find_existing_asset(
    directory: &Path,
    hash: &str,
    hash_prefix: &str,
    extension: &str,
) -> ApiResult<Option<PathBuf>> {
    let suffix = format!("-{hash_prefix}.{extension}");
    for item in fs::read_dir(directory)
        .map_err(|error| ApiError::io("Unable to scan the asset directory", error))?
    {
        let path = item
            .map_err(|error| ApiError::io("Unable to inspect an asset", error))?
            .path();
        if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.ends_with(&suffix))
        {
            let bytes = fs::read(&path)
                .map_err(|error| ApiError::io("Unable to inspect an existing asset", error))?;
            if blake3::hash(&bytes).to_hex().as_str() == hash {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

fn safe_component(value: &str) -> ApiResult<&str> {
    let mut components = Path::new(value).components();
    if matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
    {
        Ok(value)
    } else {
        Err(ApiError::new(
            "invalid_asset_path",
            "Asset identifiers cannot contain directory components.",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageDestination {
    path: String,
    range: Range<usize>,
}

fn markdown_image_patterns() -> [Regex; 2] {
    [
        Regex::new(r#"!\[[^\]\r\n]*\]\(<(?P<path>[^>\r\n]+)>(?:\s+[\"'][^)\r\n]*[\"'])?\)"#)
            .expect("valid angle image pattern"),
        Regex::new(r#"!\[[^\]\r\n]*\]\((?P<path>[^\s)\r\n]+)(?:\s+[\"'][^)\r\n]*[\"'])?\)"#)
            .expect("valid image pattern"),
    ]
}

fn html_image_pattern() -> Regex {
    Regex::new(r#"(?i)<img\b[^>\r\n]*?\bsrc\s*=\s*[\"'](?P<path>[^\"'\r\n]+)[\"'][^>]*>"#)
        .expect("valid HTML image pattern")
}

fn normalize_reference_label(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn reference_definition_patterns() -> [Regex; 2] {
    [
        Regex::new(r#"(?m)^\s{0,3}\[(?P<label>[^\]\r\n]+)\]:\s*<(?P<path>[^>\r\n]+)>"#)
            .expect("valid angle reference definition"),
        Regex::new(r#"(?m)^\s{0,3}\[(?P<label>[^\]\r\n]+)\]:\s*(?P<path>[^<\s\r\n]+)"#)
            .expect("valid reference definition"),
    ]
}

fn collect_image_destinations(content: &str) -> Vec<ImageDestination> {
    let mut destinations = Vec::new();
    let mut reference_labels = HashSet::new();
    let html_pattern = html_image_pattern();
    let parser = Parser::new_ext(content, Options::all());
    let reference_definitions: HashMap<String, Range<usize>> = parser
        .reference_definitions()
        .iter()
        .map(|(label, definition)| (normalize_reference_label(label), definition.span.clone()))
        .collect();

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Image { link_type, id, .. }) => match link_type {
                LinkType::Inline => {
                    let source = &content[range.clone()];
                    for pattern in markdown_image_patterns() {
                        let Some(captures) = pattern.captures(source) else {
                            continue;
                        };
                        let path = captures.name("path").expect("image path");
                        destinations.push(ImageDestination {
                            path: path.as_str().to_string(),
                            range: range.start + path.start()..range.start + path.end(),
                        });
                        break;
                    }
                }
                LinkType::Reference | LinkType::Collapsed | LinkType::Shortcut => {
                    reference_labels.insert(normalize_reference_label(id.as_ref()));
                }
                _ => {}
            },
            Event::Html(_) | Event::InlineHtml(_) => {
                let source = &content[range.clone()];
                for captures in html_pattern.captures_iter(source) {
                    let path = captures.name("path").expect("HTML image path");
                    destinations.push(ImageDestination {
                        path: path.as_str().to_string(),
                        range: range.start + path.start()..range.start + path.end(),
                    });
                }
            }
            _ => {}
        }
    }

    for label in reference_labels {
        let Some(range) = reference_definitions.get(&label) else {
            continue;
        };
        let source = &content[range.clone()];
        for pattern in reference_definition_patterns() {
            let Some(captures) = pattern.captures(source) else {
                continue;
            };
            let path = captures.name("path").expect("definition path");
            destinations.push(ImageDestination {
                path: path.as_str().to_string(),
                range: range.start + path.start()..range.start + path.end(),
            });
            break;
        }
    }

    destinations.sort_by_key(|destination| destination.range.start);
    destinations.dedup_by(|left, right| left.range == right.range);
    destinations
}

fn rewrite_image_destinations(content: &str, replacements: &HashMap<String, String>) -> String {
    let mut rewritten = content.to_string();
    let mut destinations = collect_image_destinations(content);
    destinations.sort_by_key(|destination| std::cmp::Reverse(destination.range.start));
    for destination in destinations {
        if let Some(replacement) = replacements.get(&destination.path) {
            rewritten.replace_range(destination.range, replacement);
        }
    }
    rewritten
}

fn asset_result(path: PathBuf, prefix: &str, pending: bool) -> WriteAssetResult {
    let filename = path.file_name().unwrap_or_default().to_string_lossy();
    let markdown_path = if pending {
        format!("inkflow-asset://{filename}")
    } else {
        format!("{prefix}/{filename}")
    };
    WriteAssetResult {
        absolute_path: path.to_string_lossy().into_owned(),
        markdown_path,
    }
}

fn mime_for_extension(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

fn is_image_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_as_copies_local_images_and_rewrites_relative_links() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("diagram.png"), b"png bytes").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "![diagram](draft.assets/diagram.png)",
        )
        .unwrap();

        assert_eq!(rewritten, "![diagram](published.assets/diagram.png)");
        assert_eq!(
            fs::read(temp.path().join("published.assets/diagram.png")).unwrap(),
            b"png bytes"
        );
    }

    #[test]
    fn save_as_only_rewrites_image_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("diagram.png"), b"png bytes").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "draft.assets/diagram.png\n![diagram](draft.assets/diagram.png)",
        )
        .unwrap();

        assert_eq!(
            rewritten,
            "draft.assets/diagram.png\n![diagram](published.assets/diagram.png)"
        );
    }

    #[test]
    fn save_as_handles_reference_and_html_images() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("one.png"), b"one").unwrap();
        fs::write(source_assets.join("two.png"), b"two").unwrap();

        let rewritten = copy_referenced_assets_for_save_as(
            &source_document,
            &destination_document,
            "![one][asset]\n![shortcut]\n\n[asset]: draft.assets/one.png\n[shortcut]: draft.assets/two.png\n<img src=\"draft.assets/two.png\">",
        )
        .unwrap();

        assert!(rewritten.contains("[asset]: published.assets/one.png"));
        assert!(rewritten.contains("[shortcut]: published.assets/two.png"));
        assert!(rewritten.contains("src=\"published.assets/two.png\""));
    }

    #[test]
    fn pending_asset_ids_cannot_escape_the_recovery_directory() {
        let temp = tempfile::tempdir().unwrap();
        let result = write_asset(
            temp.path(),
            WriteAssetRequest {
                document_id: "../outside".into(),
                document_path: None,
                source_path: None,
                data_base64: Some("aW1hZ2U=".into()),
                mime_type: Some("image/png".into()),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn save_as_does_not_rewrite_image_examples_in_code() {
        let temp = tempfile::tempdir().unwrap();
        let source_document = temp.path().join("draft.md");
        let destination_document = temp.path().join("published.md");
        let source_assets = temp.path().join("draft.assets");
        fs::create_dir(&source_assets).unwrap();
        fs::write(&source_document, "draft").unwrap();
        fs::write(source_assets.join("diagram.png"), b"png bytes").unwrap();
        let content = concat!(
            "`![inline](draft.assets/diagram.png)`\n\n",
            "```markdown\n![fenced][example]\n[example]: draft.assets/diagram.png\n```\n\n",
            "![actual][asset]\n\n[asset]: draft.assets/diagram.png\n",
        );

        let rewritten =
            copy_referenced_assets_for_save_as(&source_document, &destination_document, content)
                .unwrap();

        assert!(rewritten.contains("`![inline](draft.assets/diagram.png)`"));
        assert!(rewritten.contains("[example]: draft.assets/diagram.png"));
        assert!(rewritten.contains("[asset]: published.assets/diagram.png"));
    }

    #[test]
    fn rejects_oversized_base64_before_decoding() {
        assert!(validate_base64_payload_length(MAX_BASE64_IMAGE_BYTES).is_ok());
        assert!(validate_base64_payload_length(MAX_BASE64_IMAGE_BYTES + 1).is_err());
    }

    #[test]
    fn migration_keeps_pending_assets_until_save_commits() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        let pending = recovery.join("assets").join("document");
        fs::create_dir_all(&pending).unwrap();
        fs::write(pending.join("image.png"), b"image").unwrap();
        let document = temp.path().join("note.md");

        let rewritten = migrate_pending_assets(
            &recovery,
            "document",
            &document,
            "![image](inkflow-asset://image.png)",
        )
        .unwrap();

        assert_eq!(rewritten, "![image](note.assets/image.png)");
        assert!(pending.join("image.png").exists());
        cleanup_pending_assets(&recovery, "document").unwrap();
        assert!(!pending.exists());
    }

    #[test]
    fn migration_rejects_directory_components_as_document_ids() {
        let temp = tempfile::tempdir().unwrap();
        let recovery = temp.path().join("Recovery");
        fs::create_dir_all(recovery.join("assets")).unwrap();
        let document = temp.path().join("note.md");

        assert!(migrate_pending_assets(&recovery, ".", &document, "content").is_err());
        assert!(migrate_pending_assets(&recovery, "..", &document, "content").is_err());
        assert!(cleanup_pending_assets(&recovery, ".").is_err());
    }
}
