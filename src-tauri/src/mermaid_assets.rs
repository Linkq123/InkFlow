//! Root img/icon source ranges, matching the renderer without expanding YAML aliases.
use regex::Regex;
use saphyr_parser::{Event, Parser, ScalarStyle};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{Arc, OnceLock},
};

const MAX_SOURCE: usize = 1024 * 1024;
const MAX_DEPTH: usize = 64;

pub(crate) struct MermaidImage {
    pub source: String,
    pub range: Range<usize>,
    pub trailing_newline: bool,
    pub preserved_alias: Option<MermaidAliasEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MermaidAliasEdit {
    pub range: Range<usize>,
    pub replacement: String,
}

struct ScalarAnchor {
    value: Arc<str>,
    image: Option<usize>,
}

fn metadata_blocks(source: &str) -> Option<Vec<Range<usize>>> {
    if source.encode_utf16().count() > MAX_SOURCE {
        return None;
    }
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(offset) = source[cursor..].find("@{") else {
            break;
        };
        let start = cursor + offset + 2;
        let mut depth = 1;
        let mut nesting = 1usize;
        let mut quote = 0;
        let mut escaped = false;
        let mut index = start;
        while index < bytes.len() {
            let byte = bytes[index];
            if quote == b'"' {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quote = 0;
                }
            } else if quote == b'\'' {
                if byte == b'\'' {
                    if bytes.get(index + 1) == Some(&b'\'') {
                        index += 1;
                    } else {
                        quote = 0;
                    }
                }
            } else if byte == b'#' && index > 0 && bytes[index - 1].is_ascii_whitespace() {
                index = source[index..]
                    .find('\n')
                    .map_or(bytes.len(), |offset| index + offset);
            } else if byte == b'"' || byte == b'\'' {
                quote = byte;
            } else if byte == b'{' || byte == b'[' {
                if byte == b'{' {
                    depth += 1;
                }
                nesting += 1;
                if nesting > MAX_DEPTH {
                    return None;
                }
            } else if byte == b'}' || byte == b']' {
                nesting = nesting.saturating_sub(1);
                if byte == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
            index += 1;
        }
        if depth != 0 {
            break;
        }
        result.push(start..index);
        cursor = index + 1;
    }
    Some(result)
}

fn icon_identifier(source: &str, sequence: bool) -> bool {
    static ICON: OnceLock<Regex> = OnceLock::new();
    source.trim().starts_with('@')
        || (!sequence
            && ICON
                .get_or_init(|| {
                    Regex::new(r"(?i)^[a-z0-9]+(?:-[a-z0-9]+)*:[a-z0-9]+(?:-[a-z0-9]+)*$").unwrap()
                })
                .is_match(source.trim()))
}

pub(crate) fn collect_mermaid_images(source: &str) -> Vec<MermaidImage> {
    let Some(blocks) = metadata_blocks(source) else {
        return Vec::new();
    };
    let sequence = source
        .lines()
        .any(|line| line.trim_start().starts_with("sequenceDiagram"));
    let mut images = Vec::new();
    for block in blocks {
        if let Some(mut references) = block_images(source, block, sequence) {
            images.append(&mut references);
        }
    }
    images
}

fn block_images(source: &str, block: Range<usize>, sequence: bool) -> Option<Vec<MermaidImage>> {
    let content = &source[block.clone()];
    let prefix = if content.contains('\n') { "" } else { "{\n" };
    let yaml = format!(
        "{prefix}{content}\n{}",
        if prefix.is_empty() { "" } else { "}" }
    );
    // Parser markers count Unicode scalar values; Markdown ranges use bytes.
    let offsets: Vec<usize> = yaml
        .char_indices()
        .map(|(index, _)| index)
        .chain([yaml.len()])
        .collect();
    let mut anchors = HashMap::<usize, ScalarAnchor>::new();
    let mut keys = HashSet::new();
    let mut key = None;
    let mut expecting_key = true;
    let mut depth = 0usize;
    let mut documents = 0;
    let mut aliases = 0;
    let mut images: Vec<MermaidImage> = Vec::new();
    for parsed in Parser::new_from_str(&yaml) {
        let (event, span) = parsed.ok()?;
        let from = *offsets.get(span.start.index())?;
        let to = *offsets.get(span.end.index())?;
        let mut scalar = None;
        let mut style = ScalarStyle::Plain;
        let mut image_value = false;
        match &event {
            Event::SequenceStart(..) if depth == 0 => return None,
            Event::DocumentStart(_) => {
                documents += 1;
                if documents > 1 {
                    return None;
                }
            }
            Event::Scalar(value, scalar_style, anchor, tag) => {
                if tag.as_ref().is_some_and(|tag| {
                    !(tag.handle == "tag:yaml.org,2002:"
                        && matches!(
                            tag.suffix.as_str(),
                            "str" | "null" | "bool" | "int" | "float"
                        ))
                }) {
                    return None;
                }
                style = *scalar_style;
                let explicit_string = tag.as_ref().is_some_and(|tag| tag.suffix == "str");
                let non_string = !explicit_string
                    && (tag.is_some()
                        || (style == ScalarStyle::Plain
                            && (matches!(
                                value.as_ref(),
                                "~" | "null"
                                    | "Null"
                                    | "NULL"
                                    | "true"
                                    | "True"
                                    | "TRUE"
                                    | "false"
                                    | "False"
                                    | "FALSE"
                            ) || value.parse::<f64>().is_ok())));
                if !non_string {
                    let value: Arc<str> = Arc::from(value.as_ref());
                    scalar = Some(value.clone());
                    if *anchor > 0 {
                        anchors.insert(*anchor, ScalarAnchor { value, image: None });
                    }
                }
            }
            Event::Alias(anchor) => {
                aliases += 1;
                if aliases > 10_000 {
                    return None;
                }
                scalar = anchors.get(anchor).map(|anchor| anchor.value.clone());
            }
            Event::MappingEnd | Event::SequenceEnd => {
                depth = depth.checked_sub(1)?;
                continue;
            }
            Event::StreamStart | Event::StreamEnd | Event::DocumentEnd | Event::Nothing => continue,
            _ => {}
        }
        if depth == 1 && !matches!(event, Event::DocumentStart(_)) {
            if expecting_key {
                if let Some(value) = &scalar {
                    if !keys.insert(value.clone()) {
                        return None;
                    }
                }
                key = scalar;
            } else if key
                .as_deref()
                .is_some_and(|key| key == "img" || key == "icon")
            {
                if let Some(resource) = scalar.filter(|value| !value.trim().is_empty()) {
                    if !(key.as_deref() == Some("icon") && icon_identifier(&resource, sequence)) {
                        let block_scalar =
                            matches!(style, ScalarStyle::Literal | ScalarStyle::Folded);
                        let from = if block_scalar {
                            block_scalar_header(&yaml, from)?
                        } else {
                            from
                        };
                        // Saphyr includes the next line's indentation in a block
                        // scalar span. That belongs to the following property.
                        let to = if block_scalar {
                            let line_start = yaml[..to].rfind('\n').map_or(to, |index| index + 1);
                            if yaml[line_start..to].trim().is_empty() {
                                line_start
                            } else {
                                to
                            }
                        } else {
                            to
                        };
                        let start = block.start + from.checked_sub(prefix.len())?;
                        let end = block.end.min(block.start + to.checked_sub(prefix.len())?);
                        if let Event::Scalar(_, _, anchor, _) = &event {
                            if let Some(anchor) = anchors.get_mut(anchor) {
                                anchor.image = Some(images.len());
                            }
                        }
                        image_value = true;
                        images.push(MermaidImage {
                            source: resource.to_string(),
                            range: start..end,
                            trailing_newline: block_scalar,
                            preserved_alias: None,
                        });
                    }
                }
            }
            expecting_key = !expecting_key;
        }
        if let Event::Alias(anchor) = &event {
            if !image_value {
                if let Some(anchor) = anchors.get(anchor) {
                    if let Some(image) = anchor.image.map(|index| &mut images[index]) {
                        if image.preserved_alias.is_none() {
                            // Rebind once at the first non-image use, preserving
                            // subsequent aliases without expanding their graph.
                            image.preserved_alias = Some(MermaidAliasEdit {
                                range: block.start + from.checked_sub(prefix.len())?
                                    ..block.start + to.checked_sub(prefix.len())?,
                                replacement: format!(
                                    "&{} {}",
                                    &yaml[from + 1..to],
                                    serde_json::to_string(anchor.value.as_ref()).ok()?
                                ),
                            });
                        }
                    }
                }
            }
        }
        if matches!(event, Event::MappingStart(..) | Event::SequenceStart(..)) {
            depth += 1;
            if depth > MAX_DEPTH {
                return None;
            }
        }
    }
    Some(images)
}

fn block_scalar_header(yaml: &str, content_start: usize) -> Option<usize> {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    let regex = HEADER
        .get_or_init(|| Regex::new(r"[|>](?:[+-]?[1-9]?|[1-9][+-]?)[ \t]*(?:#.*)?$").unwrap());
    let mut end = yaml[..content_start].rfind('\n')?;
    loop {
        let start = yaml[..end].rfind('\n').map_or(0, |index| index + 1);
        if let Some(found) = regex.find(yaml[start..end].trim_end_matches('\r')) {
            return Some(start + found.start());
        }
        if start == 0 {
            return None;
        }
        end = start - 1;
    }
}

pub(crate) fn encode_mermaid_image(path: &str, trailing_newline: bool) -> String {
    let mut value = serde_json::to_string(path).expect("string serialization");
    if trailing_newline {
        value.push('\n');
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_and_excessive_metadata_stays_bounded() {
        for count in [4_000, 8_000, 16_000] {
            for source in [
                format!("flowchart LR\nA{}", "@{".repeat(count)),
                format!("A@{{img:\"{}", "@{".repeat(count)),
                format!("A@{{{}", "[".repeat(count)),
            ] {
                assert!(collect_mermaid_images(&source).is_empty());
            }
        }
        assert!(collect_mermaid_images(&format!("A@{{{}}}", "x".repeat(MAX_SOURCE))).is_empty());
        let nested = (0..80)
            .map(|depth| format!("{}nested:\n", " ".repeat(depth)))
            .collect::<String>();
        assert!(collect_mermaid_images(&format!("A@{{\nimg: x.png\n{nested}}}")).is_empty());
    }

    #[test]
    fn auxiliary_alias_graphs_are_not_expanded() {
        let mut source = "flowchart LR\nA@{\nimg: image.png\na0: &a0 [x, x]\n".to_string();
        for depth in 1..60 {
            source.push_str(&format!(
                "a{depth}: &a{depth} [*a{}, *a{}]\n",
                depth - 1,
                depth - 1
            ));
        }
        source.push('}');
        let images = collect_mermaid_images(&source);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].source, "image.png");
    }
}
