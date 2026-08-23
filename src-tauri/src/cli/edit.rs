use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};
use regex::Regex;

use crate::error::{ApiError, ApiResult};

use super::model::{
    AppliedOperation, BlockKind, DocumentEditOperation, FormatKind, MAX_DOCUMENT_EDIT_OPERATIONS,
    MAX_INLINE_FORMAT_CONTEXT_BYTES, TableAction, TextPosition, TextRange,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(content: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            content
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self { starts }
    }

    fn len(&self) -> usize {
        self.starts.len()
    }

    fn range_at(&self, content: &str, index: usize) -> Option<(usize, usize)> {
        let start = *self.starts.get(index)?;
        let end = self
            .starts
            .get(index + 1)
            .map_or(content.len(), |next| next - 1);
        Some((start, end))
    }

    fn line_range(&self, content: &str, line: usize) -> ApiResult<(usize, usize)> {
        if line == 0 {
            return Err(ApiError::new("invalid_position", "Lines start at 1."));
        }
        self.range_at(content, line - 1).ok_or_else(|| {
            ApiError::new("invalid_position", format!("Line {line} does not exist."))
        })
    }

    fn position_offset(&self, content: &str, position: TextPosition) -> ApiResult<usize> {
        if position.line == 0 || position.column == 0 {
            return Err(ApiError::new(
                "invalid_position",
                "Lines and columns start at 1.",
            ));
        }
        let (start, end) = self.line_range(content, position.line)?;
        let line = &content[start..end];
        let character_index = position.column - 1;
        if character_index == line.chars().count() {
            return Ok(end);
        }
        line.char_indices()
            .nth(character_index)
            .map(|(offset, _)| start + offset)
            .ok_or_else(|| {
                ApiError::new(
                    "invalid_position",
                    format!(
                        "Column {} is outside line {}.",
                        position.column, position.line
                    ),
                )
            })
    }

    fn resolve_range(&self, content: &str, range: TextRange) -> ApiResult<(usize, usize)> {
        let start = self.position_offset(content, range.start)?;
        let end = self.position_offset(content, range.end)?;
        if end < start {
            return Err(ApiError::new(
                "invalid_range",
                "The range end precedes its start.",
            ));
        }
        Ok((start, end))
    }

    fn apply_edit(&mut self, replaced: Range<usize>, replacement: &str) {
        let removed_length = replaced.end - replaced.start;
        let first_removed = self
            .starts
            .partition_point(|line_start| *line_start <= replaced.start);
        let after_removed = self
            .starts
            .partition_point(|line_start| *line_start <= replaced.end);
        self.starts.drain(first_removed..after_removed);

        if replacement.len() > removed_length {
            let growth = replacement.len() - removed_length;
            for line_start in self.starts.iter_mut().skip(first_removed) {
                *line_start += growth;
            }
        } else if replacement.len() < removed_length {
            let shrinkage = removed_length - replacement.len();
            for line_start in self.starts.iter_mut().skip(first_removed) {
                *line_start -= shrinkage;
            }
        }

        let inserted_starts = replacement
            .bytes()
            .enumerate()
            .filter_map(|(index, byte)| (byte == b'\n').then_some(replaced.start + index + 1));
        self.starts
            .splice(first_removed..first_removed, inserted_starts);
    }
}

#[derive(Debug)]
struct InlineBlockIndex {
    ranges: Vec<Range<usize>>,
}

impl InlineBlockIndex {
    fn new(content: &str) -> Self {
        let ranges = Parser::new_ext(content, Options::ENABLE_STRIKETHROUGH)
            .into_offset_iter()
            .filter_map(|(event, range)| {
                matches!(event, Event::Start(Tag::Paragraph | Tag::Heading { .. })).then_some(range)
            })
            .collect();
        Self { ranges }
    }

    fn context_for(
        &self,
        selection_start: usize,
        selection_end: usize,
        core_start: usize,
        core_end: usize,
    ) -> Option<(usize, Range<usize>)> {
        let index = self.ranges.partition_point(|range| range.end <= core_start);
        let range = self.ranges.get(index)?;
        (range.start <= core_start && core_end <= range.end).then(|| {
            (
                index,
                range.start.min(selection_start)..range.end.max(selection_end),
            )
        })
    }

    fn apply_inline_growth(&mut self, block_index: usize, edit_end: usize, growth: usize) {
        if growth == 0 {
            return;
        }
        if let Some(range) = self.ranges.get_mut(block_index) {
            range.end += growth;
        }
        for range in self.ranges.iter_mut().skip(block_index + 1) {
            debug_assert!(range.start >= edit_end);
            range.start += growth;
            range.end += growth;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TaskMarker {
    line: usize,
    offset: usize,
    checked: bool,
}

#[derive(Debug)]
struct TaskMarkerIndex {
    markers: Vec<TaskMarker>,
}

impl TaskMarkerIndex {
    fn new(content: &str, lines: &LineIndex) -> Self {
        let mut markers = Vec::new();
        for (event, range) in Parser::new_ext(content, Options::ENABLE_TASKLISTS).into_offset_iter()
        {
            let Event::TaskListMarker(checked) = event else {
                continue;
            };
            let line = lines
                .starts
                .partition_point(|line_start| *line_start <= range.start);
            let Some((line_start, line_end)) = line
                .checked_sub(1)
                .and_then(|index| lines.range_at(content, index))
            else {
                continue;
            };
            if range.start < line_start || range.end > line_end {
                continue;
            }
            let Some(offset) = content[range.clone()]
                .find('[')
                .map(|offset| range.start + offset + 1)
                .filter(|offset| {
                    content
                        .as_bytes()
                        .get(*offset)
                        .is_some_and(|marker| matches!(marker, b' ' | b'\t' | b'x' | b'X'))
                })
            else {
                continue;
            };
            markers.push(TaskMarker {
                line,
                offset,
                checked,
            });
        }
        Self { markers }
    }

    fn toggle(
        &mut self,
        content: &mut String,
        lines: &mut LineIndex,
        line: usize,
        checked: Option<bool>,
    ) -> ApiResult<()> {
        lines.line_range(content, line)?;
        let index = self.markers.partition_point(|marker| marker.line < line);
        let marker = self
            .markers
            .get_mut(index)
            .filter(|marker| marker.line == line)
            .ok_or_else(|| {
                ApiError::new("not_a_task", format!("Line {line} is not a Markdown task."))
            })?;
        let next = checked.unwrap_or(!marker.checked);
        replace_content_range(
            content,
            lines,
            marker.offset..marker.offset + 1,
            if next { "x" } else { " " },
        );
        marker.checked = next;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct TableLines {
    first: usize,
    last: usize,
}

#[derive(Debug)]
struct MarkdownTableIndex {
    tables: Vec<TableLines>,
}

impl MarkdownTableIndex {
    fn new(content: &str, lines: &LineIndex) -> Self {
        let tables = Parser::new_ext(content, Options::ENABLE_TABLES)
            .into_offset_iter()
            .filter_map(|(event, range)| {
                matches!(event, Event::Start(Tag::Table(_)))
                    .then(|| table_line_range(content, lines, range))
                    .flatten()
            })
            .collect();
        Self { tables }
    }

    fn byte_range_for(
        &self,
        content: &str,
        lines: &LineIndex,
        target: usize,
    ) -> Option<(usize, Range<usize>)> {
        let index = self.tables.partition_point(|table| table.last < target);
        let table = self
            .tables
            .get(index)
            .filter(|table| table.first <= target && target <= table.last)?;
        let (first_start, _) = lines.range_at(content, table.first)?;
        let (_, last_end) = lines.range_at(content, table.last)?;
        Some((index, first_start..last_end))
    }

    fn apply_replacement(&mut self, index: usize, new_line_count: usize) {
        let table = &mut self.tables[index];
        let old_line_count = table.last - table.first + 1;
        table.last = table.first + new_line_count - 1;
        if new_line_count > old_line_count {
            let growth = new_line_count - old_line_count;
            for following in self.tables.iter_mut().skip(index + 1) {
                following.first += growth;
                following.last += growth;
            }
        } else if new_line_count < old_line_count {
            let shrinkage = old_line_count - new_line_count;
            for following in self.tables.iter_mut().skip(index + 1) {
                following.first -= shrinkage;
                following.last -= shrinkage;
            }
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct EditParseStats {
    task_indexes: usize,
    table_indexes: usize,
}

pub fn apply_operations(
    content: &str,
    operations: &[DocumentEditOperation],
) -> ApiResult<(String, Vec<AppliedOperation>)> {
    let (content, operations, _) = apply_operations_internal(content, operations)?;
    Ok((content, operations))
}

fn apply_operations_internal(
    content: &str,
    operations: &[DocumentEditOperation],
) -> ApiResult<(String, Vec<AppliedOperation>, EditParseStats)> {
    if operations.len() > MAX_DOCUMENT_EDIT_OPERATIONS {
        return Err(ApiError::new(
            "too_many_operations",
            format!(
                "A document edit request supports at most {MAX_DOCUMENT_EDIT_OPERATIONS} operations."
            ),
        ));
    }
    let mut result = content.to_string();
    let mut lines = LineIndex::new(content);
    let mut applied = Vec::with_capacity(operations.len());
    let mut inline_blocks = None;
    let mut task_markers = None;
    let mut markdown_tables = None;
    let mut parse_stats = EditParseStats::default();
    for (index, operation) in operations.iter().enumerate() {
        let name = match operation {
            DocumentEditOperation::Replace {
                range,
                expected_text,
                text,
            } => {
                replace_range_with_index(&mut result, &mut lines, *range, expected_text, text)?;
                inline_blocks = None;
                task_markers = None;
                markdown_tables = None;
                "replace"
            }
            DocumentEditOperation::Format {
                range,
                expected_text,
                format,
                url,
            } => {
                format_range_with_index(
                    &mut result,
                    *range,
                    expected_text,
                    *format,
                    url.as_deref(),
                    &mut lines,
                    &mut inline_blocks,
                )?;
                task_markers = None;
                markdown_tables = None;
                "format"
            }
            DocumentEditOperation::Block { line, kind, text } => {
                replace_block_line_with_index(
                    &mut result,
                    &mut lines,
                    *line,
                    *kind,
                    text.as_deref(),
                )?;
                inline_blocks = None;
                task_markers = None;
                markdown_tables = None;
                "block"
            }
            DocumentEditOperation::ToggleTask { line, checked } => {
                if task_markers.is_none() {
                    task_markers = Some(TaskMarkerIndex::new(&result, &lines));
                    parse_stats.task_indexes += 1;
                }
                task_markers
                    .as_mut()
                    .expect("task marker index was initialized")
                    .toggle(&mut result, &mut lines, *line, *checked)?;
                inline_blocks = None;
                "toggleTask"
            }
            DocumentEditOperation::Table { line, action } => {
                if markdown_tables.is_none() {
                    markdown_tables = Some(MarkdownTableIndex::new(&result, &lines));
                    parse_stats.table_indexes += 1;
                }
                edit_table(
                    &mut result,
                    &mut lines,
                    markdown_tables
                        .as_mut()
                        .expect("Markdown table index was initialized"),
                    *line,
                    *action,
                )?;
                inline_blocks = None;
                task_markers = None;
                "table"
            }
        };
        applied.push(AppliedOperation {
            index,
            operation: name.to_string(),
        });
    }
    Ok((result, applied, parse_stats))
}

pub fn validate_position(content: &str, position: TextPosition) -> ApiResult<()> {
    LineIndex::new(content).resolve_range(
        content,
        TextRange {
            start: position,
            end: position,
        },
    )?;
    Ok(())
}

fn replace_range_with_index(
    content: &mut String,
    lines: &mut LineIndex,
    range: TextRange,
    expected: &str,
    replacement: &str,
) -> ApiResult<()> {
    let (start, end) = lines.resolve_range(content, range)?;
    if &content[start..end] != expected {
        return Err(ApiError::new(
            "expected_text_mismatch",
            "The selected text no longer matches expectedText.",
        ));
    }
    replace_content_range(content, lines, start..end, replacement);
    Ok(())
}

fn replace_content_range(
    content: &mut String,
    lines: &mut LineIndex,
    range: Range<usize>,
    replacement: &str,
) {
    content.replace_range(range.clone(), replacement);
    lines.apply_edit(range, replacement);
}

#[cfg(test)]
fn replace_range(
    content: &mut String,
    range: TextRange,
    expected: &str,
    replacement: &str,
) -> ApiResult<()> {
    let mut lines = LineIndex::new(content);
    replace_range_with_index(content, &mut lines, range, expected, replacement)
}

#[cfg(test)]
fn format_range(
    content: &mut String,
    range: TextRange,
    expected: &str,
    format: FormatKind,
    url: Option<&str>,
) -> ApiResult<()> {
    let mut lines = LineIndex::new(content);
    format_range_with_index(content, range, expected, format, url, &mut lines, &mut None)
}

fn format_range_with_index(
    content: &mut String,
    range: TextRange,
    expected: &str,
    format: FormatKind,
    url: Option<&str>,
    lines: &mut LineIndex,
    inline_blocks: &mut Option<InlineBlockIndex>,
) -> ApiResult<()> {
    if range.start.line != range.end.line {
        return Err(ApiError::new(
            "invalid_range",
            "Inline Markdown formatting must stay within one line.",
        ));
    }
    let (start, end) = lines.resolve_range(content, range)?;
    if &content[start..end] != expected {
        return Err(ApiError::new(
            "expected_text_mismatch",
            "The selected text no longer matches expectedText.",
        ));
    }
    let (replacement, formatted_block) = match format {
        FormatKind::Bold | FormatKind::Italic | FormatKind::Strike => {
            let blocks = inline_blocks.get_or_insert_with(|| InlineBlockIndex::new(content));
            let (replacement, block_index) =
                markdown_inline_format(content, start, end, expected, format, blocks)?;
            (replacement, Some(block_index))
        }
        FormatKind::Code => (markdown_code_span(expected), None),
        FormatKind::Link => (markdown_link(expected, url.unwrap_or("https://")), None),
    };
    let original_length = end - start;
    replace_content_range(content, lines, start..end, &replacement);
    if let Some(block_index) = formatted_block {
        let growth = replacement.len() - original_length;
        inline_blocks
            .as_mut()
            .expect("inline block index exists for delimiter formatting")
            .apply_inline_growth(block_index, end, growth);
    } else {
        *inline_blocks = None;
    }
    Ok(())
}

fn markdown_inline_format(
    content: &str,
    start: usize,
    end: usize,
    value: &str,
    format: FormatKind,
    inline_blocks: &InlineBlockIndex,
) -> ApiResult<(String, usize)> {
    let without_leading = value.trim_start_matches(char::is_whitespace);
    let leading_length = value.len() - without_leading.len();
    let core = without_leading.trim_end_matches(char::is_whitespace);
    if core.is_empty() {
        return Err(ApiError::new(
            "invalid_range",
            "Inline Markdown formatting requires non-whitespace selected text.",
        ));
    }

    let leading = &value[..leading_length];
    let trailing = &value[leading_length + core.len()..];
    let core_start = start + leading.len();
    let core_end = core_start + core.len();
    let (block_index, context) = inline_blocks
        .context_for(start, end, core_start, core_end)
        .ok_or_else(|| {
            ApiError::new(
                "invalid_range",
                "Inline Markdown formatting must select text inside a paragraph or heading.",
            )
        })?;
    if context.len() > MAX_INLINE_FORMAT_CONTEXT_BYTES {
        return Err(ApiError::new(
            "format_context_too_large",
            format!(
                "Inline Markdown formatting supports a containing block of at most {MAX_INLINE_FORMAT_CONTEXT_BYTES} bytes."
            ),
        ));
    }
    let markers: &[&str] = match format {
        FormatKind::Bold => &["**", "__"],
        FormatKind::Italic => &["*", "_"],
        FormatKind::Strike => &["~~"],
        FormatKind::Code | FormatKind::Link => unreachable!("inline delimiter format"),
    };

    // Prefer the original Markdown unchanged so valid nested formatting remains nested. If a
    // delimiter in the selection would close the new span early, try the equivalent delimiter
    // and finally a literal-preserving escaped form.
    for escape_inner in [false, true] {
        for marker in markers {
            let inner = if escape_inner {
                escape_markdown_delimiter(core, marker.chars().next().expect("format marker"))
            } else {
                core.to_string()
            };
            let replacement = format!("{leading}{marker}{inner}{marker}{trailing}");
            let tag_start = start - context.start + leading.len();
            let tag_end = tag_start + marker.len() + inner.len() + marker.len();
            let mut candidate =
                String::with_capacity(context.len() - (end - start) + replacement.len());
            candidate.push_str(&content[context.start..start]);
            candidate.push_str(&replacement);
            candidate.push_str(&content[end..context.end]);

            if Parser::new_ext(&candidate, Options::ENABLE_STRIKETHROUGH)
                .into_offset_iter()
                .any(|(event, range)| {
                    range == (tag_start..tag_end)
                        && matches!(
                            (format, event),
                            (FormatKind::Bold, Event::Start(Tag::Strong))
                                | (FormatKind::Italic, Event::Start(Tag::Emphasis))
                                | (FormatKind::Strike, Event::Start(Tag::Strikethrough))
                        )
                })
            {
                return Ok((replacement, block_index));
            }
        }
    }

    Err(ApiError::new(
        "invalid_range",
        "The selected text cannot be safely represented with the requested Markdown format.",
    ))
}

fn escape_markdown_delimiter(value: &str, delimiter: char) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if character == '\\' || character == delimiter {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn replace_block_line_with_index(
    content: &mut String,
    lines: &mut LineIndex,
    line: usize,
    kind: BlockKind,
    requested_text: Option<&str>,
) -> ApiResult<()> {
    let (start, end) = lines.line_range(content, line)?;
    let current = content[start..end].trim();
    let text = requested_text.unwrap_or_else(|| {
        if current.starts_with('/') {
            ""
        } else {
            current
        }
    });
    let replacement = match kind {
        BlockKind::Heading1 => format!("# {text}"),
        BlockKind::Heading2 => format!("## {text}"),
        BlockKind::Heading3 => format!("### {text}"),
        BlockKind::BulletList => format!("- {text}"),
        BlockKind::Task => format!("- [ ] {text}"),
        BlockKind::Quote => format!("> {text}"),
        BlockKind::CodeBlock => markdown_code_block(text),
        BlockKind::MathBlock => format!("$$\n{text}\n$$"),
    };
    replace_content_range(content, lines, start..end, &replacement);
    Ok(())
}

#[cfg(test)]
fn replace_block_line(
    content: &mut String,
    line: usize,
    kind: BlockKind,
    requested_text: Option<&str>,
) -> ApiResult<()> {
    let mut lines = LineIndex::new(content);
    replace_block_line_with_index(content, &mut lines, line, kind, requested_text)
}

fn markdown_code_span(value: &str) -> String {
    let fence = "`".repeat(longest_character_run(value, '`') + 1);
    let is_all_spaces = !value.is_empty() && value.chars().all(|character| character == ' ');
    let needs_padding = value.starts_with('`')
        || value.ends_with('`')
        || (value.starts_with(' ') && value.ends_with(' ') && !is_all_spaces);
    if needs_padding {
        format!("{fence} {value} {fence}")
    } else {
        format!("{fence}{value}{fence}")
    }
}

fn markdown_code_block(value: &str) -> String {
    let fence = "`".repeat((longest_character_run(value, '`') + 1).max(3));
    let trailing_newline = if value.ends_with('\n') { "" } else { "\n" };
    format!("{fence}\n{value}{trailing_newline}{fence}")
}

fn markdown_link(label: &str, destination: &str) -> String {
    let mut escaped_label = String::with_capacity(label.len());
    for character in label.chars() {
        if character == '&' {
            escaped_label.push_str("&amp;");
        } else {
            if matches!(character, '\\' | '[' | ']') {
                escaped_label.push('\\');
            }
            escaped_label.push(character);
        }
    }

    let mut escaped_destination = String::with_capacity(destination.len() + 2);
    for character in destination.chars() {
        if character.is_control() {
            let mut encoded = [0u8; 4];
            for byte in character.encode_utf8(&mut encoded).as_bytes() {
                escaped_destination.push_str(&format!("%{byte:02X}"));
            }
        } else if character == '&' {
            escaped_destination.push_str("&amp;");
        } else {
            if matches!(character, '\\' | '<' | '>') {
                escaped_destination.push('\\');
            }
            escaped_destination.push(character);
        }
    }
    format!("[{escaped_label}](<{escaped_destination}>)")
}

fn longest_character_run(value: &str, delimiter: char) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in value.chars() {
        if character == delimiter {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn edit_table(
    content: &mut String,
    lines: &mut LineIndex,
    tables: &mut MarkdownTableIndex,
    target_line: usize,
    action: TableAction,
) -> ApiResult<()> {
    if target_line == 0 || target_line > lines.len() {
        return Err(ApiError::new(
            "invalid_position",
            "The table line is outside the document.",
        ));
    }
    let target = target_line - 1;
    let (table_index, source_range) =
        tables
            .byte_range_for(content, lines, target)
            .ok_or_else(|| {
                ApiError::new(
                    "not_a_table",
                    "The requested line is not inside a Markdown table.",
                )
            })?;
    let source = &content[source_range.clone()];
    let transformed = transform_markdown_table(source, action)?;
    let new_line_count = transformed.bytes().filter(|byte| *byte == b'\n').count() + 1;
    replace_content_range(content, lines, source_range, &transformed);
    tables.apply_replacement(table_index, new_line_count);
    Ok(())
}

fn table_line_range(
    content: &str,
    lines: &LineIndex,
    parser_range: Range<usize>,
) -> Option<TableLines> {
    let first = lines
        .starts
        .partition_point(|line_start| *line_start <= parser_range.start)
        .checked_sub(1)?;
    let mut last = lines
        .starts
        .partition_point(|line_start| *line_start < parser_range.end)
        .checked_sub(1)?;
    for index in (first + 2)..=last {
        let (start, end) = lines.range_at(content, index)?;
        if starts_non_table_block(&content[start..end]) {
            last = index - 1;
            break;
        }
    }
    Some(TableLines { first, last })
}

fn starts_non_table_block(line: &str) -> bool {
    if line.starts_with("    ") || line.starts_with('\t') {
        return true;
    }
    let trimmed = line.trim_start();
    trimmed == ">"
        || trimmed.starts_with("> ")
        || trimmed.starts_with("# ")
        || trimmed.starts_with("## ")
        || trimmed.starts_with("### ")
        || trimmed.starts_with("```")
        || trimmed.starts_with("~~~")
        || ["- ", "+ ", "* "]
            .iter()
            .any(|marker| trimmed.starts_with(marker))
        || starts_ordered_list(trimmed)
}

fn starts_ordered_list(line: &str) -> bool {
    let bytes = line.as_bytes();
    let digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    (1..=9).contains(&digits)
        && matches!(bytes.get(digits), Some(b'.' | b')'))
        && bytes
            .get(digits + 1)
            .is_some_and(|byte| byte.is_ascii_whitespace())
}

pub fn transform_markdown_table(source: &str, action: TableAction) -> ApiResult<String> {
    let mut rows: Vec<Vec<String>> = source.lines().map(split_table_row).collect();
    if rows.len() < 2
        || rows[0].is_empty()
        || rows[0].len() != rows[1].len()
        || !is_separator_row(&rows[1])
    {
        return Err(ApiError::new(
            "not_a_table",
            "The Markdown table separator is invalid.",
        ));
    }
    let columns = rows[0].len();
    for row in &mut rows {
        row.resize(columns, String::new());
    }
    match action {
        TableAction::AddRow => rows.push(vec![String::new(); columns]),
        TableAction::RemoveRow if rows.len() > 2 => {
            rows.pop();
        }
        TableAction::RemoveRow => {
            return Err(ApiError::new(
                "table_limit",
                "A table must keep its header and separator.",
            ));
        }
        TableAction::AddColumn => {
            for (index, row) in rows.iter_mut().enumerate() {
                row.push(if index == 1 {
                    "---".to_string()
                } else {
                    String::new()
                });
            }
        }
        TableAction::RemoveColumn if columns > 1 => {
            for row in &mut rows {
                row.pop();
            }
        }
        TableAction::RemoveColumn => {
            return Err(ApiError::new(
                "table_limit",
                "A table must keep at least one column.",
            ));
        }
    }
    Ok(rows
        .into_iter()
        .map(|row| format!("| {} |", row.join(" | ")))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn split_table_row(row: &str) -> Vec<String> {
    let mut source = row.trim();
    if let Some(without_leading_delimiter) = source.strip_prefix('|') {
        source = without_leading_delimiter;
    }
    if source.ends_with('|') && !is_escaped(source, source.len() - 1) {
        source = &source[..source.len() - 1];
    }
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in source.chars() {
        if character == '|' && !escaped {
            cells.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(character);
        }
        escaped = character == '\\' && !escaped;
        if character != '\\' {
            escaped = false;
        }
    }
    cells.push(current.trim().to_string());
    cells
}

fn is_escaped(source: &str, byte_index: usize) -> bool {
    source.as_bytes()[..byte_index]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count()
        % 2
        == 1
}

fn is_separator_row(row: &[String]) -> bool {
    let pattern = Regex::new(r"^:?-{3,}:?$").expect("valid table separator");
    !row.is_empty() && row.iter().all(|cell| pattern.is_match(cell.trim()))
}

#[cfg(test)]
mod tests {
    use super::super::model::{DocumentEditOperation, TextPosition, TextRange};
    use super::*;
    use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};

    #[test]
    fn positions_count_unicode_characters() {
        let range = TextRange {
            start: TextPosition { line: 1, column: 2 },
            end: TextPosition { line: 1, column: 4 },
        };
        let mut value = "你a好b".to_string();
        replace_range(&mut value, range, "a好", "Ink").unwrap();
        assert_eq!(value, "你Inkb");
    }

    #[test]
    fn applies_operations_sequentially() {
        let operations = vec![
            DocumentEditOperation::Replace {
                range: TextRange {
                    start: TextPosition { line: 1, column: 1 },
                    end: TextPosition { line: 1, column: 4 },
                },
                expected_text: "old".into(),
                text: "new".into(),
            },
            DocumentEditOperation::ToggleTask {
                line: 2,
                checked: Some(true),
            },
        ];
        let (value, applied) = apply_operations("old\n- [ ] task", &operations).unwrap();
        assert_eq!(value, "new\n- [x] task");
        assert_eq!(applied.len(), 2);
    }

    #[test]
    fn sequential_edits_reuse_an_index_that_tracks_inserted_and_removed_lines() {
        let operations = [
            DocumentEditOperation::Replace {
                range: TextRange {
                    start: TextPosition { line: 1, column: 2 },
                    end: TextPosition { line: 1, column: 3 },
                },
                expected_text: "a".into(),
                text: "A\ninserted".into(),
            },
            DocumentEditOperation::Replace {
                range: TextRange {
                    start: TextPosition { line: 2, column: 9 },
                    end: TextPosition { line: 3, column: 1 },
                },
                expected_text: "\n".into(),
                text: " ".into(),
            },
            DocumentEditOperation::Format {
                range: TextRange {
                    start: TextPosition { line: 3, column: 1 },
                    end: TextPosition { line: 3, column: 6 },
                },
                expected_text: "third".into(),
                format: FormatKind::Bold,
                url: None,
            },
        ];

        let (value, applied) = apply_operations("你a\nsecond\nthird", &operations).unwrap();

        assert_eq!(value, "你A\ninserted second\n**third**");
        assert_eq!(applied.len(), operations.len());
    }

    #[test]
    fn incremental_line_index_matches_a_rebuild_after_each_edit() {
        let mut value = "alpha\nbeta\nlast".to_string();
        let mut lines = LineIndex::new(&value);

        replace_content_range(&mut value, &mut lines, 6..6, "新行\n");
        assert_eq!(lines, LineIndex::new(&value));

        let newline = lines
            .resolve_range(
                &value,
                TextRange {
                    start: TextPosition { line: 2, column: 3 },
                    end: TextPosition { line: 3, column: 1 },
                },
            )
            .unwrap();
        replace_content_range(&mut value, &mut lines, newline.0..newline.1, " ");
        assert_eq!(lines, LineIndex::new(&value));

        let value_length = value.len();
        replace_content_range(&mut value, &mut lines, 0..value_length, "done\n");
        assert_eq!(lines, LineIndex::new(&value));
    }

    #[test]
    fn rejects_edit_batches_over_the_public_operation_limit() {
        let operations = (0..=MAX_DOCUMENT_EDIT_OPERATIONS)
            .map(|_| DocumentEditOperation::ToggleTask {
                line: 1,
                checked: Some(true),
            })
            .collect::<Vec<_>>();

        let error = apply_operations("- [ ] task", &operations).unwrap_err();

        assert_eq!(error.code, "too_many_operations");
    }

    #[test]
    fn applies_multiple_inline_formats_with_one_updated_block_index() {
        let operations = [
            DocumentEditOperation::Format {
                range: TextRange {
                    start: TextPosition { line: 1, column: 1 },
                    end: TextPosition { line: 1, column: 4 },
                },
                expected_text: "one".into(),
                format: FormatKind::Bold,
                url: None,
            },
            DocumentEditOperation::Format {
                range: TextRange {
                    start: TextPosition { line: 1, column: 9 },
                    end: TextPosition {
                        line: 1,
                        column: 12,
                    },
                },
                expected_text: "two".into(),
                format: FormatKind::Italic,
                url: None,
            },
        ];

        let (value, applied) = apply_operations("one two three", &operations).unwrap();

        assert_eq!(value, "**one** *two* three");
        assert_eq!(applied.len(), 2);
    }

    #[test]
    fn inline_code_uses_a_delimiter_longer_than_the_selected_content() {
        let mut value = "`code`".to_string();
        format_range(
            &mut value,
            TextRange {
                start: TextPosition { line: 1, column: 1 },
                end: TextPosition { line: 1, column: 7 },
            },
            "`code`",
            FormatKind::Code,
            None,
        )
        .unwrap();

        assert_eq!(value, "`` `code` ``");
        assert!(
            Parser::new(&value)
                .any(|event| matches!(event, Event::Code(code) if code.as_ref() == "`code`"))
        );
    }

    #[test]
    fn links_preserve_labels_and_destinations_with_character_references() {
        let label = "a&copy;]b";
        let mut value = label.to_string();
        let destination = "https://example.com/a_(b)?first=1&copy;=2";
        format_range(
            &mut value,
            TextRange {
                start: TextPosition { line: 1, column: 1 },
                end: TextPosition {
                    line: 1,
                    column: label.chars().count() + 1,
                },
            },
            label,
            FormatKind::Link,
            Some(destination),
        )
        .unwrap();

        assert_eq!(
            value,
            "[a&amp;copy;\\]b](<https://example.com/a_(b)?first=1&amp;copy;=2>)"
        );
        let events = Parser::new(&value).collect::<Vec<_>>();
        assert!(events.iter().any(|event| {
            matches!(event, Event::Start(Tag::Link { dest_url, .. }) if dest_url.as_ref() == destination)
        }));
        let parsed_label = events
            .iter()
            .filter_map(|event| match event {
                Event::Text(text) => Some(text.as_ref()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(parsed_label, label);
    }

    #[test]
    fn emphasis_formats_the_entire_selection_when_it_contains_delimiters() {
        let cases = [
            ("a**b", FormatKind::Bold, "__a**b__", Tag::Strong),
            ("a*b", FormatKind::Italic, "_a*b_", Tag::Emphasis),
            (
                "a~~b",
                FormatKind::Strike,
                "~~a\\~\\~b~~",
                Tag::Strikethrough,
            ),
        ];

        for (original, format, expected, expected_tag) in cases {
            let mut value = original.to_string();
            format_range(
                &mut value,
                TextRange {
                    start: TextPosition { line: 1, column: 1 },
                    end: TextPosition {
                        line: 1,
                        column: original.chars().count() + 1,
                    },
                },
                original,
                format,
                None,
            )
            .unwrap();

            assert_eq!(value, expected);
            assert!(
                Parser::new_ext(&value, Options::ENABLE_STRIKETHROUGH)
                    .into_offset_iter()
                    .any(|(event, range)| {
                        matches!(event, Event::Start(tag) if tag == expected_tag)
                            && range == (0..value.len())
                    })
            );
        }
    }

    #[test]
    fn emphasis_keeps_selected_whitespace_outside_the_delimiters() {
        let original = "  InkFlow \t";
        let mut value = original.to_string();

        format_range(
            &mut value,
            TextRange {
                start: TextPosition { line: 1, column: 1 },
                end: TextPosition {
                    line: 1,
                    column: original.chars().count() + 1,
                },
            },
            original,
            FormatKind::Bold,
            None,
        )
        .unwrap();

        assert_eq!(value, "  **InkFlow** \t");
        assert!(
            Parser::new(&value)
                .into_offset_iter()
                .any(|(event, range)| {
                    matches!(event, Event::Start(Tag::Strong)) && range == (2..13)
                })
        );
    }

    #[test]
    fn emphasis_rejects_empty_and_whitespace_only_selections_without_mutating_content() {
        for (original, range, selected) in [
            (
                "InkFlow",
                TextRange {
                    start: TextPosition { line: 1, column: 1 },
                    end: TextPosition { line: 1, column: 1 },
                },
                "",
            ),
            (
                "   ",
                TextRange {
                    start: TextPosition { line: 1, column: 1 },
                    end: TextPosition { line: 1, column: 4 },
                },
                "   ",
            ),
        ] {
            let mut value = original.to_string();
            let error =
                format_range(&mut value, range, selected, FormatKind::Bold, None).unwrap_err();

            assert_eq!(error.code, "invalid_range");
            assert_eq!(value, original);
        }
    }

    #[test]
    fn inline_formatting_parses_only_the_selected_block_in_a_large_document() {
        let archived = "x".repeat(5 * 1024 * 1024);
        let original = format!("{archived}\n\nformat me");
        let start = archived.len() + 2;
        let blocks = InlineBlockIndex::new(&original);
        let (_, context) = blocks
            .context_for(start, original.len(), start, original.len())
            .unwrap();

        assert_eq!(&original[context.clone()], "format me");
        assert_eq!(context.len(), "format me".len());

        let mut value = original;
        format_range(
            &mut value,
            TextRange {
                start: TextPosition { line: 3, column: 1 },
                end: TextPosition {
                    line: 3,
                    column: 10,
                },
            },
            "format me",
            FormatKind::Bold,
            None,
        )
        .unwrap();
        assert!(value.ends_with("\n\n**format me**"));
    }

    #[test]
    fn inline_formatting_rejects_an_oversized_markdown_block() {
        let original = "x".repeat(MAX_INLINE_FORMAT_CONTEXT_BYTES + 1);
        let mut value = original.clone();

        let error = format_range(
            &mut value,
            TextRange {
                start: TextPosition { line: 1, column: 1 },
                end: TextPosition { line: 1, column: 2 },
            },
            "x",
            FormatKind::Bold,
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "format_context_too_large");
        assert_eq!(value, original);
    }

    #[test]
    fn inline_formatting_rejects_text_inside_a_fenced_code_block() {
        let original = "```\nword\n```";
        let mut value = original.to_string();

        let error = format_range(
            &mut value,
            TextRange {
                start: TextPosition { line: 2, column: 1 },
                end: TextPosition { line: 2, column: 5 },
            },
            "word",
            FormatKind::Italic,
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "invalid_range");
        assert_eq!(value, original);
    }

    #[test]
    fn inline_formatting_rejects_ranges_that_cross_markdown_blocks() {
        let original = "first\n\nsecond";
        let mut value = original.to_string();

        let error = format_range(
            &mut value,
            TextRange {
                start: TextPosition { line: 1, column: 1 },
                end: TextPosition { line: 3, column: 7 },
            },
            original,
            FormatKind::Bold,
            None,
        )
        .unwrap_err();

        assert_eq!(error.code, "invalid_range");
        assert_eq!(value, original);
    }

    #[test]
    fn code_blocks_use_a_fence_longer_than_fences_in_the_content() {
        let mut value = "placeholder".to_string();
        let code = "alpha\n```\nomega";
        replace_block_line(&mut value, 1, BlockKind::CodeBlock, Some(code)).unwrap();

        assert_eq!(value, "````\nalpha\n```\nomega\n````");
        assert!(Parser::new(&value).any(|event| {
            matches!(
                event,
                Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_)))
            )
        }));
    }

    #[test]
    fn toggles_gfm_tasks_in_ordered_lists() {
        let operations = [
            DocumentEditOperation::ToggleTask {
                line: 1,
                checked: Some(true),
            },
            DocumentEditOperation::ToggleTask {
                line: 2,
                checked: Some(false),
            },
        ];

        let (value, applied) =
            apply_operations("1. [ ] first\n  2) [X] second", &operations).unwrap();

        assert_eq!(value, "1. [x] first\n  2) [ ] second");
        assert_eq!(applied.len(), 2);
    }

    #[test]
    fn task_toggle_rejects_indented_code_blocks() {
        let original = "    - [ ] sample code";
        let operation = [DocumentEditOperation::ToggleTask {
            line: 1,
            checked: Some(true),
        }];

        let error = apply_operations(original, &operation).unwrap_err();

        assert_eq!(error.code, "not_a_task");
    }

    #[test]
    fn task_toggle_accepts_tasks_nested_in_block_quotes() {
        let operation = [DocumentEditOperation::ToggleTask {
            line: 1,
            checked: Some(true),
        }];

        let (value, _) = apply_operations("> - [ ] quoted task", &operation).unwrap();

        assert_eq!(value, "> - [x] quoted task");
    }

    #[test]
    fn batched_task_toggles_build_the_markdown_index_once() {
        let content = (1..=10_000)
            .map(|line| format!("- [ ] task {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let operations = (0..MAX_DOCUMENT_EDIT_OPERATIONS)
            .map(|_| DocumentEditOperation::ToggleTask {
                line: 9_999,
                checked: None,
            })
            .collect::<Vec<_>>();

        let (edited, applied, stats) = apply_operations_internal(&content, &operations).unwrap();

        assert_eq!(edited, content);
        assert_eq!(applied.len(), MAX_DOCUMENT_EDIT_OPERATIONS);
        assert_eq!(stats.task_indexes, 1);
        assert_eq!(stats.table_indexes, 0);
    }

    #[test]
    fn structural_edits_invalidate_the_cached_task_index() {
        let operations = [
            DocumentEditOperation::ToggleTask {
                line: 2,
                checked: Some(true),
            },
            DocumentEditOperation::Replace {
                range: TextRange {
                    start: TextPosition { line: 1, column: 1 },
                    end: TextPosition { line: 1, column: 1 },
                },
                expected_text: String::new(),
                text: "prefix\n".into(),
            },
            DocumentEditOperation::ToggleTask {
                line: 3,
                checked: Some(false),
            },
        ];

        let (edited, _, stats) =
            apply_operations_internal("title\n- [ ] task", &operations).unwrap();

        assert_eq!(edited, "prefix\ntitle\n- [ ] task");
        assert_eq!(stats.task_indexes, 2);
    }

    #[test]
    fn table_commands_match_the_editor_behavior() {
        let table = "| Name | Ready |\n| --- | :---: |\n| InkFlow | yes |";
        let added = transform_markdown_table(table, TableAction::AddColumn).unwrap();
        assert!(added.contains("| --- | :---: | --- |"));
        assert_eq!(
            transform_markdown_table(&added, TableAction::RemoveColumn).unwrap(),
            table
        );
    }

    #[test]
    fn table_commands_preserve_an_escaped_pipe_in_the_last_cell() {
        let table = "| Key | Value |\n| --- | --- |\n| Pipe | A \\||";

        let added = transform_markdown_table(table, TableAction::AddColumn).unwrap();

        assert_eq!(added.lines().nth(2), Some("| Pipe | A \\| |  |"));
        assert_eq!(
            transform_markdown_table(&added, TableAction::RemoveColumn).unwrap(),
            "| Key | Value |\n| --- | --- |\n| Pipe | A \\| |"
        );
    }

    #[test]
    fn table_edit_starts_at_the_header_after_pipe_prose() {
        let content = "See A | B\n| Name | Ready |\n| --- | :---: |\n| InkFlow | yes |";
        let operations = [DocumentEditOperation::Table {
            line: 2,
            action: TableAction::AddColumn,
        }];

        let (edited, _) = apply_operations(content, &operations).unwrap();

        assert_eq!(edited.lines().next(), Some("See A | B"));
        assert!(edited.contains("| Name | Ready |  |"));
        assert!(edited.contains("| --- | :---: | --- |"));
    }

    #[test]
    fn table_edit_does_not_absorb_following_markdown_blocks_with_pipes() {
        for trailing in [
            "> quoted | text",
            "- list item | text",
            "    indented | code",
        ] {
            let content = format!("| Name | Ready |\n| --- | --- |\n| InkFlow | yes |\n{trailing}");
            let operations = [DocumentEditOperation::Table {
                line: 3,
                action: TableAction::AddColumn,
            }];

            let (edited, _) = apply_operations(&content, &operations).unwrap();

            assert_eq!(edited.lines().last(), Some(trailing));
            assert!(edited.contains("| InkFlow | yes |  |"));
        }
    }

    #[test]
    fn batched_table_edits_build_the_markdown_index_once() {
        let content = "| Name | Ready |\n| --- | --- |\n| InkFlow | yes |";
        let operations = (0..MAX_DOCUMENT_EDIT_OPERATIONS)
            .map(|index| DocumentEditOperation::Table {
                line: 1,
                action: if index % 2 == 0 {
                    TableAction::AddRow
                } else {
                    TableAction::RemoveRow
                },
            })
            .collect::<Vec<_>>();

        let (edited, applied, stats) = apply_operations_internal(content, &operations).unwrap();

        assert_eq!(edited, content);
        assert_eq!(applied.len(), MAX_DOCUMENT_EDIT_OPERATIONS);
        assert_eq!(stats.task_indexes, 0);
        assert_eq!(stats.table_indexes, 1);
    }

    #[test]
    fn cached_table_ranges_follow_line_changes_in_earlier_tables() {
        let content = "| A |\n| --- |\n| one |\n\n| B |\n| --- |\n| two |";
        let operations = [
            DocumentEditOperation::Table {
                line: 1,
                action: TableAction::AddRow,
            },
            DocumentEditOperation::Table {
                line: 6,
                action: TableAction::AddColumn,
            },
        ];

        let (edited, _, stats) = apply_operations_internal(content, &operations).unwrap();

        assert!(edited.contains("| B |  |\n| --- | --- |\n| two |  |"));
        assert_eq!(stats.table_indexes, 1);
    }
}
