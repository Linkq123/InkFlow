use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DiskRevision {
    #[ts(type = "number")]
    pub modified_ms: u64,
    #[ts(type = "number")]
    pub size: u64,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DocumentSnapshot {
    pub id: String,
    pub path: Option<String>,
    pub title: String,
    pub content: String,
    pub encoding: String,
    pub eol: String,
    pub had_bom: bool,
    pub had_final_newline: bool,
    pub read_only: bool,
    pub revision: Option<DiskRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SaveDocumentRequest {
    pub id: String,
    pub path: Option<String>,
    pub title: String,
    pub content: String,
    pub encoding: String,
    pub eol: String,
    pub had_bom: bool,
    pub expected_revision: Option<DiskRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "camelCase")]
#[ts(tag = "status", rename_all = "camelCase")]
pub enum SaveOutcome {
    Saved {
        path: String,
        revision: DiskRevision,
        content: Option<String>,
    },
    Conflict {
        path: String,
        #[serde(rename = "diskRevision")]
        #[ts(rename = "diskRevision")]
        disk_revision: Option<DiskRevision>,
    },
    NeedsPath,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExternalChange {
    pub document_id: String,
    pub path: String,
    pub kind: String,
    pub revision: Option<DiskRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    pub root: String,
    pub name: String,
    pub entries: Vec<WorkspaceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SearchRequest {
    pub root: String,
    pub query: String,
    pub case_sensitive: bool,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SearchHit {
    pub path: String,
    pub relative_path: String,
    pub line: u32,
    pub column: u32,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WriteAssetRequest {
    pub document_id: String,
    pub document_path: Option<String>,
    pub source_path: Option<String>,
    pub data_base64: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct WriteAssetResult {
    pub absolute_path: String,
    pub markdown_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExportRequest {
    pub title: String,
    pub rendered_html: String,
    pub output_path: Option<String>,
    pub page_size: Option<String>,
    pub landscape: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ExportOutcome {
    pub action: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RecoveryEntry {
    pub id: String,
    pub document_id: String,
    pub path: Option<String>,
    pub title: String,
    pub created_at: String,
    pub kind: String,
    pub size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RecoverySnapshot {
    pub entry: RecoveryEntry,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct CheckpointRequest {
    pub document_id: String,
    pub path: Option<String>,
    pub title: String,
    pub content: String,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SettingsV1 {
    pub schema_version: u32,
    pub locale: String,
    pub theme: String,
    pub page_width: u32,
    pub font_size: u32,
    pub line_height: f32,
    pub editor_font: String,
    pub code_font: String,
    pub autosave_delay_ms: u32,
    pub show_file_tree: bool,
    pub show_outline: bool,
    pub focus_mode: bool,
    pub typewriter_mode: bool,
    pub recent_files: Vec<String>,
    pub recent_workspaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SessionTabV1 {
    pub path: String,
    pub mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct SessionV1 {
    pub schema_version: u32,
    pub workspace_root: Option<String>,
    pub tabs: Vec<SessionTabV1>,
    pub active_path: Option<String>,
}

impl Default for SessionV1 {
    fn default() -> Self {
        Self {
            schema_version: 1,
            workspace_root: None,
            tabs: Vec::new(),
            active_path: None,
        }
    }
}

impl Default for SettingsV1 {
    fn default() -> Self {
        Self {
            schema_version: 1,
            locale: "system".into(),
            theme: "system".into(),
            page_width: 820,
            font_size: 16,
            line_height: 1.75,
            editor_font: "Segoe UI Variable, Microsoft YaHei UI, sans-serif".into(),
            code_font: "Cascadia Mono, Consolas, monospace".into(),
            autosave_delay_ms: 750,
            show_file_tree: false,
            show_outline: false,
            focus_mode: false,
            typewriter_mode: false,
            recent_files: Vec::new(),
            recent_workspaces: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryRecord {
    pub entry: RecoveryEntry,
    pub content: String,
    pub hash: String,
}
