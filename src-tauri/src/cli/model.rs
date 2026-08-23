use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::{ApiError, ApiResult},
    model::{DiskRevision, SessionTabV1, SessionV1},
};

pub const CLI_API_VERSION: &str = "inkflow.cli/v1";
pub const MAX_DOCUMENT_EDIT_OPERATIONS: usize = 256;
pub const MAX_INLINE_FORMAT_CONTEXT_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CliEnvelope<T> {
    #[schemars(extend("const" = CLI_API_VERSION))]
    pub api_version: &'static str,
    #[schemars(extend("const" = true))]
    pub ok: bool,
    pub command: String,
    pub data: T,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CliErrorEnvelope {
    #[schemars(extend("const" = CLI_API_VERSION))]
    pub api_version: &'static str,
    #[schemars(extend("const" = false))]
    pub ok: bool,
    pub command: String,
    pub error: CliError,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CliError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitiesV1 {
    #[schemars(extend("const" = CLI_API_VERSION))]
    pub api_version: &'static str,
    pub product: &'static str,
    pub version: &'static str,
    pub platform: &'static str,
    pub output_formats: Vec<&'static str>,
    pub commands: BTreeMap<&'static str, Vec<&'static str>>,
    pub safety: CapabilitySafetyV1,
    pub limits: CapabilityLimitsV1,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySafetyV1 {
    pub remote_images_default: &'static str,
    pub atomic_writes: bool,
    pub revision_conflicts: bool,
    pub workspace_symlinks: &'static str,
    pub destructive_confirmation: &'static str,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityLimitsV1 {
    pub asset_bytes: u64,
    pub workspace_search_hits: u32,
    pub document_edit_operations: usize,
    pub inline_format_context_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CliDiskRevision {
    #[schemars(extend("format" = "date-time"))]
    pub modified_at: String,
    pub size: u64,
    pub hash: String,
}

fn revision_time(modified_ms: u64) -> String {
    let modified_ms = i64::try_from(modified_ms).expect("filesystem timestamp fits in i64");
    DateTime::<Utc>::from_timestamp_millis(modified_ms)
        .expect("filesystem timestamp is representable as RFC 3339")
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl CliDiskRevision {
    pub fn to_disk_revision(&self) -> ApiResult<DiskRevision> {
        let modified_at = DateTime::parse_from_rfc3339(&self.modified_at).map_err(|_| {
            ApiError::new(
                "invalid_revision",
                "Revision modifiedAt must be an RFC 3339 timestamp.",
            )
        })?;
        let modified_ms = modified_at.timestamp_millis();
        if modified_ms < 0 {
            return Err(ApiError::new(
                "invalid_revision",
                "Revision modifiedAt cannot be before the Unix epoch.",
            ));
        }
        Ok(DiskRevision {
            modified_ms: modified_ms as u64,
            size: self.size,
            hash: self.hash.clone(),
        })
    }
}

impl From<DiskRevision> for CliDiskRevision {
    fn from(value: DiskRevision) -> Self {
        Self {
            modified_at: revision_time(value.modified_ms),
            size: value.size,
            hash: value.hash,
        }
    }
}

impl From<&DiskRevision> for CliDiskRevision {
    fn from(value: &DiskRevision) -> Self {
        Self {
            modified_at: revision_time(value.modified_ms),
            size: value.size,
            hash: value.hash.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentInfo {
    pub path: Option<String>,
    pub title: String,
    pub content: String,
    pub encoding: String,
    pub eol: String,
    pub had_bom: bool,
    pub had_final_newline: bool,
    pub read_only: bool,
    pub revision: Option<CliDiskRevision>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DocumentStats {
    pub words: usize,
    pub lines: usize,
    pub characters: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OutlineItem {
    pub level: u8,
    pub text: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentAnalysis {
    pub stats: DocumentStats,
    pub outline: Vec<OutlineItem>,
    pub has_remote_images: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TextPosition {
    #[schemars(range(min = 1))]
    pub line: usize,
    #[schemars(range(min = 1))]
    pub column: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TextRange {
    pub start: TextPosition,
    pub end: TextPosition,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DocumentEditRequestV1 {
    #[serde(default = "default_schema_version")]
    #[schemars(extend("const" = 1))]
    pub schema_version: u32,
    pub expected_revision: Option<CliDiskRevision>,
    #[schemars(length(max = MAX_DOCUMENT_EDIT_OPERATIONS))]
    pub operations: Vec<DocumentEditOperation>,
}

fn default_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DocumentEditOperation {
    Replace {
        range: TextRange,
        expected_text: String,
        text: String,
    },
    Format {
        range: TextRange,
        expected_text: String,
        format: FormatKind,
        url: Option<String>,
    },
    Block {
        #[schemars(range(min = 1))]
        line: usize,
        kind: BlockKind,
        text: Option<String>,
    },
    ToggleTask {
        #[schemars(range(min = 1))]
        line: usize,
        checked: Option<bool>,
    },
    Table {
        #[schemars(range(min = 1))]
        line: usize,
        action: TableAction,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FormatKind {
    Bold,
    Italic,
    Strike,
    Code,
    Link,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    Heading1,
    Heading2,
    Heading3,
    BulletList,
    Task,
    Quote,
    CodeBlock,
    MathBlock,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TableAction {
    AddRow,
    RemoveRow,
    AddColumn,
    RemoveColumn,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AppliedOperation {
    pub index: usize,
    pub operation: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentMutationOutcome {
    pub path: String,
    pub changed: bool,
    pub dry_run: bool,
    pub previous_revision: Option<CliDiskRevision>,
    pub revision: Option<CliDiskRevision>,
    pub content_hash: String,
    pub operations: Vec<AppliedOperation>,
    pub changed_ranges: Vec<TextRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatchV1 {
    pub locale: Option<String>,
    #[schemars(extend("enum" = [(), "system", "light", "dark"]))]
    pub theme: Option<String>,
    pub page_width: Option<u32>,
    pub font_size: Option<u32>,
    pub line_height: Option<f32>,
    pub editor_font: Option<String>,
    pub code_font: Option<String>,
    pub autosave_delay_ms: Option<u32>,
    pub show_file_tree: Option<bool>,
    pub show_outline: Option<bool>,
    pub focus_mode: Option<bool>,
    pub typewriter_mode: Option<bool>,
    pub recent_files: Option<Vec<String>>,
    pub recent_workspaces: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenderRequestV1 {
    #[serde(default = "default_schema_version")]
    #[schemars(extend("const" = 1))]
    pub schema_version: u32,
    pub title: String,
    pub markdown: String,
    pub document_path: Option<String>,
    #[serde(default)]
    pub allow_remote_images: bool,
    pub page_size: Option<String>,
    pub landscape: Option<bool>,
}

/// A single schema root keeps every shared definition in one `$defs` map.
/// The optional properties are a discoverable catalog, not a runtime payload.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CliSchemaCatalogV1 {
    pub capabilities: Option<CapabilitiesV1>,
    pub success_envelope: Option<CliEnvelope<Value>>,
    pub error_envelope: Option<CliErrorEnvelope>,
    pub document_info: Option<DocumentInfo>,
    pub document_analysis: Option<DocumentAnalysis>,
    pub document_mutation_outcome: Option<DocumentMutationOutcome>,
    pub document_edit_request: Option<DocumentEditRequestV1>,
    pub settings_patch: Option<SettingsPatchV1>,
    pub session: Option<SessionV1>,
    pub session_tab: Option<SessionTabV1>,
    pub render_request: Option<RenderRequestV1>,
    pub cli_error: Option<CliError>,
}
