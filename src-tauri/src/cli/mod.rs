mod edit;
mod model;
mod renderer_client;
mod service;

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(not(windows))]
use std::process::{Command as ProcessCommand, Stdio};

use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{Args, Parser, Subcommand, ValueEnum, error::ErrorKind};
use regex::RegexBuilder;
use schemars::schema_for;
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    asset,
    error::ApiError,
    export, fileio,
    model::{CheckpointRequest, SearchRequest, SessionV1, SettingsV1},
    session::{MAX_SESSION_TABS, SessionStore},
    settings::SettingsStore,
    workspace::WorkspaceStore,
};

use edit::{apply_operations, validate_position};
use model::{
    AppliedOperation, CLI_API_VERSION, CapabilitiesV1, CapabilityLimitsV1, CapabilitySafetyV1,
    CliDiskRevision, CliEnvelope, CliError, CliErrorEnvelope, CliSchemaCatalogV1,
    DocumentEditOperation, DocumentEditRequestV1, MAX_DOCUMENT_EDIT_OPERATIONS,
    MAX_INLINE_FORMAT_CONTEXT_BYTES, SettingsPatchV1, TextPosition, TextRange,
};
use service::{CliContext, WriteOptions};

static CANCELLED: AtomicBool = AtomicBool::new(false);
const MAX_STDIN_ASSET_BYTES: u64 = 50 * 1024 * 1024;
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CANCELLATION_CLEANUP_GRACE: Duration = Duration::from_millis(500);

pub(super) fn cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

#[derive(Debug, PartialEq, Eq)]
enum InterruptibleError {
    Cancelled,
    WorkerStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancellationPolicy {
    /// The operation is read-only and may be abandoned after its cleanup window.
    AbandonAfterGrace,
    /// The operation may hold an atomic-write or rollback guard, so the process
    /// must remain alive until Rust has run its cleanup path.
    FinishBeforeExit,
}

fn run_interruptibly<T, F, C>(
    operation: F,
    is_cancelled: C,
    poll_interval: Duration,
    cleanup_grace: Duration,
    cancellation_policy: CancellationPolicy,
) -> Result<T, InterruptibleError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    C: Fn() -> bool,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = sender.send(operation());
    });

    loop {
        match receiver.recv_timeout(poll_interval) {
            Ok(result) => return Ok(result),
            Err(RecvTimeoutError::Disconnected) => {
                return Err(InterruptibleError::WorkerStopped);
            }
            Err(RecvTimeoutError::Timeout) if is_cancelled() => {
                return match receiver.recv_timeout(cleanup_grace) {
                    Ok(result) => Ok(result),
                    Err(RecvTimeoutError::Disconnected) => Err(InterruptibleError::WorkerStopped),
                    Err(RecvTimeoutError::Timeout)
                        if cancellation_policy == CancellationPolicy::FinishBeforeExit =>
                    {
                        receiver
                            .recv()
                            .map_err(|_| InterruptibleError::WorkerStopped)
                    }
                    Err(RecvTimeoutError::Timeout) => Err(InterruptibleError::Cancelled),
                };
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
enum OutputFormat {
    Auto,
    Text,
    Json,
    Jsonl,
}

#[derive(Parser)]
#[command(
    name = "inkflow-cli",
    version,
    about = "Headless InkFlow Markdown tools for people and agents",
    arg_required_else_help = true
)]
struct Cli {
    /// Output format. Auto uses text on a terminal and JSON when piped.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    format: OutputFormat,

    /// Use an isolated InkFlow application data directory.
    #[arg(long, global = true, value_name = "PATH")]
    data_dir: Option<PathBuf>,

    /// Restrict every referenced path to this directory and reject reparse points.
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<PathBuf>,

    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Subcommand)]
enum TopCommand {
    /// Describe the stable CLI surface for automated clients.
    Capabilities,
    /// Print the JSON Schema for versioned request types.
    Schema,
    /// Read, inspect and safely mutate Markdown documents.
    Document {
        #[command(subcommand)]
        command: DocumentCommand,
    },
    /// Inspect and mutate a bounded workspace.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    /// Add or safely load document image assets.
    Asset {
        #[command(subcommand)]
        command: AssetCommand,
    },
    /// Manage InkFlow recovery snapshots.
    Recovery {
        #[command(subcommand)]
        command: RecoveryCommand,
    },
    /// Read and patch shared InkFlow settings.
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
    /// Read and update the next desktop session.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Render Markdown to a sanitized HTML fragment.
    Render {
        #[command(subcommand)]
        command: RenderCommand,
    },
    /// Export Markdown through the InkFlow rendering pipeline.
    Export {
        #[command(subcommand)]
        command: ExportCommand,
    },
    /// Start the desktop application without controlling a running window.
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
}

#[derive(Subcommand)]
enum DocumentCommand {
    Read(SourceArgs),
    Analyze(SourceArgs),
    Search(DocumentSearchArgs),
    Replace(DocumentReplaceArgs),
    Edit(DocumentEditArgs),
    Write(DocumentWriteArgs),
    SaveAs(DocumentSaveAsArgs),
}

#[derive(Args)]
struct SourceArgs {
    /// Markdown path, or - to read UTF-8 from stdin.
    #[arg(value_name = "PATH|-", default_value = "-")]
    source: PathBuf,
}

#[derive(Args)]
struct DocumentSearchArgs {
    path: PathBuf,
    query: String,
    #[arg(long)]
    case_sensitive: bool,
    #[arg(long)]
    regex: bool,
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u32).range(1..=2000))]
    limit: u32,
}

#[derive(Args)]
struct DocumentReplaceArgs {
    path: PathBuf,
    query: String,
    replacement: String,
    #[arg(long)]
    case_sensitive: bool,
    #[arg(long)]
    regex: bool,
    #[arg(long)]
    all: bool,
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u32).range(1..=100000))]
    max_replacements: u32,
    #[arg(long)]
    expected_hash: Option<String>,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct DocumentEditArgs {
    path: PathBuf,
    /// JSON request path, or - to read from stdin.
    #[arg(long, value_name = "PATH|-", default_value = "-")]
    request: PathBuf,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct DocumentWriteArgs {
    path: PathBuf,
    /// UTF-8 content path, or - to read from stdin.
    #[arg(long, value_name = "PATH|-", default_value = "-")]
    input: PathBuf,
    #[arg(long)]
    expected_hash: Option<String>,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    create: bool,
    #[arg(long)]
    encoding: Option<String>,
    #[arg(long, value_parser = ["lf", "crlf"])]
    eol: Option<String>,
    #[arg(long, conflicts_with = "no_bom")]
    bom: bool,
    #[arg(long, conflicts_with = "bom")]
    no_bom: bool,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct DocumentSaveAsArgs {
    source: PathBuf,
    destination: PathBuf,
    #[arg(long)]
    expected_destination_hash: Option<String>,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
enum WorkspaceCommand {
    Tree(WorkspaceRootArgs),
    Search(WorkspaceSearchArgs),
    Create(WorkspaceCreateArgs),
    Rename(WorkspaceRenameArgs),
    Trash(WorkspaceTrashArgs),
}

#[derive(Args)]
struct WorkspaceRootArgs {
    #[arg(value_name = "ROOT")]
    workspace_root: PathBuf,
}

#[derive(Args)]
struct WorkspaceSearchArgs {
    #[arg(value_name = "ROOT")]
    workspace_root: PathBuf,
    query: String,
    #[arg(long)]
    case_sensitive: bool,
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u32).range(1..=2000))]
    limit: u32,
}

#[derive(Args)]
struct WorkspaceCreateArgs {
    #[arg(value_name = "ROOT")]
    workspace_root: PathBuf,
    parent: PathBuf,
    name: String,
    #[arg(long)]
    directory: bool,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct WorkspaceRenameArgs {
    #[arg(value_name = "ROOT")]
    workspace_root: PathBuf,
    path: PathBuf,
    new_name: String,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct WorkspaceTrashArgs {
    #[arg(value_name = "ROOT")]
    workspace_root: PathBuf,
    path: PathBuf,
    #[arg(long)]
    yes: bool,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Subcommand)]
enum AssetCommand {
    Add(AssetAddArgs),
    Read(AssetReadArgs),
}

#[derive(Args)]
struct AssetAddArgs {
    #[arg(
        long,
        conflicts_with = "document_id",
        required_unless_present = "document_id"
    )]
    document: Option<PathBuf>,
    #[arg(
        long,
        conflicts_with = "document",
        required_unless_present = "document"
    )]
    document_id: Option<String>,
    #[arg(long, conflicts_with = "stdin")]
    source: Option<PathBuf>,
    #[arg(long, conflicts_with = "source")]
    stdin: bool,
    #[arg(long, default_value = "image/png")]
    mime_type: String,
    #[arg(long, requires = "document", value_parser = clap::value_parser!(u32).range(1..))]
    line: Option<u32>,
    #[arg(long, requires = "line", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    column: u32,
    #[arg(long, default_value = "image")]
    alt: String,
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct AssetReadArgs {
    #[arg(long)]
    document: PathBuf,
    resource: String,
    #[arg(long)]
    workspace: Option<PathBuf>,
}

#[derive(Subcommand)]
enum RecoveryCommand {
    List,
    Checkpoint(RecoveryCheckpointArgs),
    Restore(RecoveryRestoreArgs),
    Delete(RecoveryDeleteArgs),
}

#[derive(Args)]
struct RecoveryCheckpointArgs {
    path: PathBuf,
    #[arg(long, default_value = "history", value_parser = ["draft", "history"])]
    kind: String,
    #[arg(long)]
    document_id: Option<String>,
}

#[derive(Args)]
struct RecoveryRestoreArgs {
    id: String,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    expected_hash: Option<String>,
    #[arg(long)]
    force: bool,
    #[arg(long)]
    create: bool,
}

#[derive(Args)]
struct RecoveryDeleteArgs {
    id: String,
    #[arg(long)]
    yes: bool,
}

#[derive(Subcommand)]
enum SettingsCommand {
    Get,
    Patch(JsonInputArgs),
    Reset,
}

#[derive(Subcommand)]
enum SessionCommand {
    Get,
    Update(SessionUpdateArgs),
    Clear(SessionClearArgs),
}

#[derive(Args)]
struct JsonInputArgs {
    #[arg(long, value_name = "PATH|-", default_value = "-")]
    input: PathBuf,
}

#[derive(Args)]
struct SessionUpdateArgs {
    #[arg(long, value_name = "PATH|-", default_value = "-")]
    input: PathBuf,
    #[arg(long)]
    expected_hash: Option<String>,
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct SessionClearArgs {
    #[arg(long)]
    yes: bool,
}

#[derive(Subcommand)]
enum RenderCommand {
    Fragment(RenderArgs),
}

#[derive(Args)]
struct RenderArgs {
    #[command(flatten)]
    source: SourceArgs,
    #[arg(long)]
    document_path: Option<PathBuf>,
    #[arg(long)]
    allow_remote_images: bool,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}

#[derive(Subcommand)]
enum ExportCommand {
    Html(ExportArgs),
    Pdf(ExportArgs),
}

#[derive(Args)]
struct ExportArgs {
    source: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    title: Option<String>,
    #[arg(long, default_value = "A4", value_parser = ["A4", "Letter"])]
    page_size: String,
    #[arg(long)]
    landscape: bool,
    #[arg(long)]
    allow_remote_images: bool,
    #[arg(long)]
    expected_output_hash: Option<String>,
    #[arg(long)]
    force: bool,
}

#[derive(Subcommand)]
enum AppCommand {
    Open(AppOpenArgs),
}

#[derive(Args)]
struct AppOpenArgs {
    paths: Vec<PathBuf>,
    #[arg(long)]
    workspace: Option<PathBuf>,
}

struct CommandOutput {
    command: String,
    data: Value,
    human: String,
    warnings: Vec<String>,
    stream_items: Option<Vec<Value>>,
    streamed_count: Option<usize>,
    exit_code: i32,
}

impl CommandOutput {
    fn new(
        command: impl Into<String>,
        data: impl Serialize,
        human: impl Into<String>,
    ) -> Result<Self, CliFailure> {
        Ok(Self {
            command: command.into(),
            data: serde_json::to_value(data).map_err(CliFailure::serialization)?,
            human: human.into(),
            warnings: Vec::new(),
            stream_items: None,
            streamed_count: None,
            exit_code: 0,
        })
    }

    fn warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    fn stream(mut self, items: Vec<Value>) -> Self {
        self.stream_items = Some(items);
        self
    }

    fn partial(mut self) -> Self {
        self.exit_code = 6;
        self
    }

    fn already_streamed(mut self, count: usize) -> Self {
        self.streamed_count = Some(count);
        self
    }
}

#[derive(Debug)]
struct CliFailure {
    command: String,
    error: CliError,
    exit_code: i32,
}

impl CliFailure {
    fn new(code: impl Into<String>, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            command: "inkflow-cli".into(),
            error: CliError {
                code: code.into(),
                message: message.into(),
                details: None,
            },
            exit_code,
        }
    }

    fn for_command(mut self, command: impl Into<String>) -> Self {
        self.command = command.into();
        self
    }

    fn serialization(error: serde_json::Error) -> Self {
        Self::new("serialization_error", error.to_string(), 3)
    }
}

impl From<ApiError> for CliFailure {
    fn from(value: ApiError) -> Self {
        let exit_code = match value.code.as_str() {
            "revision_conflict" | "expected_text_mismatch" => 4,
            "confirmation_required" => 5,
            "invalid_data_directory"
            | "invalid_bom"
            | "invalid_eol"
            | "unsupported_encoding"
            | "invalid_settings"
            | "invalid_name"
            | "invalid_position"
            | "invalid_range"
            | "too_many_operations"
            | "format_context_too_large"
            | "invalid_path"
            | "not_a_file"
            | "missing_document"
            | "missing_asset"
            | "missing_path"
            | "missing_output_path" => 2,
            _ => 3,
        };
        Self::new(value.code, value.message, exit_code)
    }
}

fn cancelled_failure(command: impl Into<String>) -> CliFailure {
    CliFailure::new(
        "cancelled",
        "The command was interrupted before it could complete.",
        3,
    )
    .for_command(command)
}

pub fn run() -> i32 {
    CANCELLED.store(false, Ordering::SeqCst);
    let args: Vec<String> = std::env::args().collect();
    let requested_format = format_from_raw_args(&args);
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return 0;
        }
        Err(error) => {
            let failure = CliFailure::new("invalid_arguments", error.to_string(), 2);
            return emitted_exit(
                emit_failure(&failure, resolve_format(requested_format)),
                failure.exit_code,
            );
        }
    };
    let format = resolve_format(cli.format);
    let command_name = top_command_name(&cli.command).to_string();
    if let Err(error) = ctrlc::set_handler(|| CANCELLED.store(true, Ordering::SeqCst)) {
        let failure = CliFailure::new(
            "signal_handler_error",
            format!("Unable to register the Ctrl+C cleanup handler: {error}"),
            3,
        )
        .for_command(command_name.clone());
        return emitted_exit(emit_failure(&failure, format), failure.exit_code);
    }
    if let Some(result) = execute_without_context(&cli.command) {
        return emit_command_result(result, format);
    }
    let context = match CliContext::new(cli.data_dir, cli.root) {
        Ok(context) => context,
        Err(error) => {
            let failure = CliFailure::from(error).for_command(command_name.clone());
            return emitted_exit(emit_failure(&failure, format), failure.exit_code);
        }
    };
    let cancellation_policy = cancellation_policy(&cli.command);
    let command = match (format, cli.command) {
        (
            OutputFormat::Jsonl,
            TopCommand::Workspace {
                command: WorkspaceCommand::Search(args),
            },
        ) => return run_workspace_search_jsonl(args, context, format),
        (_, command) => command,
    };
    // Keep stdout ownership on this thread. A read-only worker may be abandoned
    // after the cleanup grace period, so it must never outlive a locked stdout.
    let execution = run_interruptibly(
        move || execute(command, &context),
        cancelled,
        CANCELLATION_POLL_INTERVAL,
        CANCELLATION_CLEANUP_GRACE,
        cancellation_policy,
    );
    emit_command_result(resolve_execution(execution, command_name), format)
}

fn resolve_execution(
    execution: Result<Result<CommandOutput, CliFailure>, InterruptibleError>,
    command_name: String,
) -> Result<CommandOutput, CliFailure> {
    match execution {
        Ok(result) => result,
        Err(InterruptibleError::Cancelled) => Err(cancelled_failure(command_name)),
        Err(InterruptibleError::WorkerStopped) => Err(CliFailure::new(
            "worker_stopped",
            "The command worker stopped without returning a result.",
            3,
        )
        .for_command(command_name)),
    }
}

fn run_workspace_search_jsonl(
    args: WorkspaceSearchArgs,
    context: CliContext,
    format: OutputFormat,
) -> i32 {
    // Search remains incremental, but the worker only sends serializable items.
    // The caller is the sole stdout writer for items, summaries and failures.
    let (item_sender, item_receiver) = mpsc::channel();
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result =
            workspace_search_command(args, &context, Some(&item_sender)).map_err(|failure| {
                if failure.command == "inkflow-cli" {
                    failure.for_command("workspace.search")
                } else {
                    failure
                }
            });
        let _ = result_sender.send(result);
    });

    let mut cancellation_deadline = None;
    loop {
        if let Err(error) = emit_pending_jsonl_items(&item_receiver) {
            return emitted_exit(Err(error), 3);
        }
        match result_receiver.recv_timeout(CANCELLATION_POLL_INTERVAL) {
            Ok(result) => {
                if let Err(error) = emit_pending_jsonl_items(&item_receiver) {
                    return emitted_exit(Err(error), 3);
                }
                return emit_command_result(result, format);
            }
            Err(RecvTimeoutError::Disconnected) => {
                let failure = CliFailure::new(
                    "worker_stopped",
                    "The command worker stopped without returning a result.",
                    3,
                )
                .for_command("workspace.search");
                return emit_command_result(Err(failure), format);
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        if cancelled() {
            let deadline = cancellation_deadline
                .get_or_insert_with(|| Instant::now() + CANCELLATION_CLEANUP_GRACE);
            if Instant::now() >= *deadline {
                return emit_command_result(Err(cancelled_failure("workspace.search")), format);
            }
        }
    }
}

fn execute_without_context(command: &TopCommand) -> Option<Result<CommandOutput, CliFailure>> {
    let (command_name, result) = match command {
        TopCommand::Capabilities => ("capabilities", capabilities()),
        TopCommand::Schema => ("schema", schema()),
        _ => return None,
    };
    Some(result.map_err(|failure| {
        if failure.command == "inkflow-cli" {
            failure.for_command(command_name)
        } else {
            failure
        }
    }))
}

fn emit_command_result(result: Result<CommandOutput, CliFailure>, format: OutputFormat) -> i32 {
    match result {
        Ok(output) => {
            let exit_code = output.exit_code;
            emitted_exit(emit_success(output, format), exit_code)
        }
        Err(failure) if failure.error.code == "broken_pipe" => 0,
        Err(failure) => emitted_exit(emit_failure(&failure, format), failure.exit_code),
    }
}

fn execute(command: TopCommand, context: &CliContext) -> Result<CommandOutput, CliFailure> {
    let command_name = top_command_name(&command);
    let result = match command {
        TopCommand::Capabilities => capabilities(),
        TopCommand::Schema => schema(),
        TopCommand::Document { command } => document_command(command, context),
        TopCommand::Workspace { command } => workspace_command(command, context),
        TopCommand::Asset { command } => asset_command(command, context),
        TopCommand::Recovery { command } => recovery_command(command, context),
        TopCommand::Settings { command } => settings_command(command, context),
        TopCommand::Session { command } => session_command(command, context),
        TopCommand::Render { command } => render_command(command, context),
        TopCommand::Export { command } => export_command(command, context),
        TopCommand::App { command } => app_command(command, context),
    };
    result.map_err(|failure| {
        if failure.command == "inkflow-cli" {
            failure.for_command(command_name)
        } else {
            failure
        }
    })
}

fn top_command_name(command: &TopCommand) -> &'static str {
    match command {
        TopCommand::Capabilities => "capabilities",
        TopCommand::Schema => "schema",
        TopCommand::Document { command } => match command {
            DocumentCommand::Read(_) => "document.read",
            DocumentCommand::Analyze(_) => "document.analyze",
            DocumentCommand::Search(_) => "document.search",
            DocumentCommand::Replace(_) => "document.replace",
            DocumentCommand::Edit(_) => "document.edit",
            DocumentCommand::Write(_) => "document.write",
            DocumentCommand::SaveAs(_) => "document.saveAs",
        },
        TopCommand::Workspace { command } => match command {
            WorkspaceCommand::Tree(_) => "workspace.tree",
            WorkspaceCommand::Search(_) => "workspace.search",
            WorkspaceCommand::Create(_) => "workspace.create",
            WorkspaceCommand::Rename(_) => "workspace.rename",
            WorkspaceCommand::Trash(_) => "workspace.trash",
        },
        TopCommand::Asset { command } => match command {
            AssetCommand::Add(_) => "asset.add",
            AssetCommand::Read(_) => "asset.read",
        },
        TopCommand::Recovery { command } => match command {
            RecoveryCommand::List => "recovery.list",
            RecoveryCommand::Checkpoint(_) => "recovery.checkpoint",
            RecoveryCommand::Restore(_) => "recovery.restore",
            RecoveryCommand::Delete(_) => "recovery.delete",
        },
        TopCommand::Settings { command } => match command {
            SettingsCommand::Get => "settings.get",
            SettingsCommand::Patch(_) => "settings.patch",
            SettingsCommand::Reset => "settings.reset",
        },
        TopCommand::Session { command } => match command {
            SessionCommand::Get => "session.get",
            SessionCommand::Update(_) => "session.update",
            SessionCommand::Clear(_) => "session.clear",
        },
        TopCommand::Render { command } => match command {
            RenderCommand::Fragment(_) => "render.fragment",
        },
        TopCommand::Export { command } => match command {
            ExportCommand::Html(_) => "export.html",
            ExportCommand::Pdf(_) => "export.pdf",
        },
        TopCommand::App { command } => match command {
            AppCommand::Open(_) => "app.open",
        },
    }
}

fn cancellation_policy(command: &TopCommand) -> CancellationPolicy {
    match command {
        TopCommand::Capabilities | TopCommand::Schema => CancellationPolicy::AbandonAfterGrace,
        TopCommand::Document { command } => match command {
            DocumentCommand::Read(_) | DocumentCommand::Analyze(_) | DocumentCommand::Search(_) => {
                CancellationPolicy::AbandonAfterGrace
            }
            DocumentCommand::Replace(_)
            | DocumentCommand::Edit(_)
            | DocumentCommand::Write(_)
            | DocumentCommand::SaveAs(_) => CancellationPolicy::FinishBeforeExit,
        },
        TopCommand::Workspace { command } => match command {
            WorkspaceCommand::Tree(_) | WorkspaceCommand::Search(_) => {
                CancellationPolicy::AbandonAfterGrace
            }
            WorkspaceCommand::Create(_)
            | WorkspaceCommand::Rename(_)
            | WorkspaceCommand::Trash(_) => CancellationPolicy::FinishBeforeExit,
        },
        TopCommand::Asset { command } => match command {
            AssetCommand::Read(_) => CancellationPolicy::AbandonAfterGrace,
            AssetCommand::Add(_) => CancellationPolicy::FinishBeforeExit,
        },
        TopCommand::Recovery { command } => match command {
            RecoveryCommand::List => CancellationPolicy::AbandonAfterGrace,
            RecoveryCommand::Checkpoint(_)
            | RecoveryCommand::Restore(_)
            | RecoveryCommand::Delete(_) => CancellationPolicy::FinishBeforeExit,
        },
        TopCommand::Settings { command } => match command {
            SettingsCommand::Get => CancellationPolicy::AbandonAfterGrace,
            SettingsCommand::Patch(_) | SettingsCommand::Reset => {
                CancellationPolicy::FinishBeforeExit
            }
        },
        TopCommand::Session { command } => match command {
            SessionCommand::Get => CancellationPolicy::AbandonAfterGrace,
            SessionCommand::Update(_) | SessionCommand::Clear(_) => {
                CancellationPolicy::FinishBeforeExit
            }
        },
        // Renderer commands own a private request directory and may also have
        // an atomic output replacement in flight. app.open creates an external
        // process, whose actual launch result must not be reported as cancelled.
        TopCommand::Render { .. } | TopCommand::Export { .. } | TopCommand::App { .. } => {
            CancellationPolicy::FinishBeforeExit
        }
    }
}

fn capabilities() -> Result<CommandOutput, CliFailure> {
    let data = CapabilitiesV1 {
        api_version: CLI_API_VERSION,
        product: "InkFlow",
        version: env!("CARGO_PKG_VERSION"),
        platform: "windows-x64",
        output_formats: vec!["auto", "text", "json", "jsonl"],
        commands: BTreeMap::from([
            ("capabilities", vec![]),
            ("schema", vec![]),
            (
                "document",
                vec![
                    "read", "analyze", "search", "replace", "edit", "write", "save-as",
                ],
            ),
            (
                "workspace",
                vec!["tree", "search", "create", "rename", "trash"],
            ),
            ("asset", vec!["add", "read"]),
            ("recovery", vec!["list", "checkpoint", "restore", "delete"]),
            ("settings", vec!["get", "patch", "reset"]),
            ("session", vec!["get", "update", "clear"]),
            ("render", vec!["fragment"]),
            ("export", vec!["html", "pdf"]),
            ("app", vec!["open"]),
        ]),
        safety: CapabilitySafetyV1 {
            remote_images_default: "blocked",
            atomic_writes: true,
            revision_conflicts: true,
            workspace_symlinks: "blocked",
            destructive_confirmation: "--yes",
        },
        limits: CapabilityLimitsV1 {
            asset_bytes: 52_428_800,
            workspace_search_hits: 2_000,
            document_edit_operations: MAX_DOCUMENT_EDIT_OPERATIONS,
            inline_format_context_bytes: MAX_INLINE_FORMAT_CONTEXT_BYTES,
        },
    };
    let human = serde_json::to_string_pretty(&data).map_err(CliFailure::serialization)?;
    CommandOutput::new("capabilities", data, human)
}

fn schema() -> Result<CommandOutput, CliFailure> {
    let mut data =
        serde_json::to_value(schema_for!(CliSchemaCatalogV1)).map_err(CliFailure::serialization)?;
    let root = data.as_object_mut().ok_or_else(|| {
        CliFailure::new(
            "schema_generation_error",
            "The generated CLI schema root is not an object.",
            3,
        )
    })?;
    root.insert(
        "$id".into(),
        Value::String(
            "https://github.com/Linkq123/InkFlow/blob/master/docs/inkflow-cli.schema.json".into(),
        ),
    );
    root.insert(
        "title".into(),
        Value::String("InkFlow CLI v1 contract catalog".into()),
    );
    root.insert("apiVersion".into(), Value::String(CLI_API_VERSION.into()));
    let human = serde_json::to_string_pretty(&data).map_err(CliFailure::serialization)?;
    CommandOutput::new("schema", data, human)
}

fn document_command(
    command: DocumentCommand,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    match command {
        DocumentCommand::Read(args) => {
            let document = read_source(context, &args.source)?;
            let human = document.content.clone();
            CommandOutput::new("document.read", document, human)
        }
        DocumentCommand::Analyze(args) => {
            let document = read_source(context, &args.source)?;
            let analysis = service::analyze_document(&document.content);
            let human = format!(
                "{} words · {} lines · {} characters · {} headings",
                analysis.stats.words,
                analysis.stats.lines,
                analysis.stats.characters,
                analysis.outline.len()
            );
            CommandOutput::new("document.analyze", analysis, human)
        }
        DocumentCommand::Search(args) => document_search(args, context),
        DocumentCommand::Replace(args) => document_replace(args, context),
        DocumentCommand::Edit(args) => document_edit(args, context),
        DocumentCommand::Write(args) => document_write(args, context),
        DocumentCommand::SaveAs(args) => {
            let outcome = service::save_document_as(
                context,
                &args.source,
                &args.destination,
                args.expected_destination_hash.as_deref(),
                args.force,
                args.dry_run,
            )
            .map_err(CliFailure::from)?;
            let human = mutation_human(&outcome);
            CommandOutput::new("document.saveAs", outcome, human)
        }
    }
}

fn document_search(
    args: DocumentSearchArgs,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    if !args.regex && args.query.is_empty() {
        return Err(CliFailure::new(
            "invalid_query",
            "The literal search query cannot be empty. Use --regex to opt in to a zero-width pattern.",
            2,
        )
        .for_command("document.search"));
    }
    let document = service::read_document(context, &args.path).map_err(CliFailure::from)?;
    let pattern = if args.regex {
        RegexBuilder::new(&args.query)
            .case_insensitive(!args.case_sensitive)
            .build()
            .map_err(|error| CliFailure::new("invalid_regex", error.to_string(), 2))?
    } else {
        RegexBuilder::new(&regex::escape(&args.query))
            .case_insensitive(!args.case_sensitive)
            .build()
            .map_err(|error| CliFailure::new("invalid_query", error.to_string(), 2))?
    };
    let mut hits = Vec::new();
    // `split` preserves the logical first line of an empty document and the
    // final empty line after a trailing newline. Those lines are observable by
    // the CLI's explicitly supported zero-width regular expressions.
    for (line_index, line) in document.content.split('\n').enumerate() {
        for found in pattern.find_iter(line) {
            hits.push(json!({
                "path": document.path,
                "line": line_index + 1,
                "column": line[..found.start()].chars().count() + 1,
                "endColumn": line[..found.end()].chars().count() + 1,
                "preview": line.trim().chars().take(240).collect::<String>()
            }));
            if hits.len() >= args.limit as usize {
                break;
            }
        }
        if hits.len() >= args.limit as usize {
            break;
        }
    }
    let human = hits
        .iter()
        .map(|hit| {
            format!(
                "{}:{}: {}",
                hit["line"],
                hit["column"],
                hit["preview"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    CommandOutput::new(
        "document.search",
        json!({ "hits": hits, "count": hits.len() }),
        human,
    )
    .map(|output| output.stream(hits))
}

fn document_replace(
    args: DocumentReplaceArgs,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    if !args.regex && args.query.is_empty() {
        return Err(CliFailure::new(
            "invalid_query",
            "The literal replacement query cannot be empty. Use --regex to opt in to a zero-width pattern.",
            2,
        )
        .for_command("document.replace"));
    }
    let document = service::read_document(context, &args.path).map_err(CliFailure::from)?;
    if let Some(expected) = args.expected_hash.as_deref() {
        if document.revision.as_ref().map(|value| value.hash.as_str()) != Some(expected) {
            return Err(
                CliFailure::new("revision_conflict", "The document hash has changed.", 4)
                    .for_command("document.replace"),
            );
        }
    }
    let pattern = if args.regex {
        RegexBuilder::new(&args.query)
            .case_insensitive(!args.case_sensitive)
            .build()
            .map_err(|error| CliFailure::new("invalid_regex", error.to_string(), 2))?
    } else {
        RegexBuilder::new(&regex::escape(&args.query))
            .case_insensitive(!args.case_sensitive)
            .build()
            .map_err(|error| CliFailure::new("invalid_query", error.to_string(), 2))?
    };
    let maximum = if args.all {
        args.max_replacements as usize
    } else {
        1
    };
    let mut count = 0;
    let mut truncated = false;
    let replaced = pattern
        .replace_all(&document.content, |captures: &regex::Captures<'_>| {
            if count >= maximum {
                if args.all {
                    truncated = true;
                }
                captures.get(0).expect("whole match").as_str().to_string()
            } else {
                count += 1;
                if args.regex {
                    let mut expanded = String::new();
                    captures.expand(&args.replacement, &mut expanded);
                    expanded
                } else {
                    args.replacement.clone()
                }
            }
        })
        .into_owned();
    let operations = if count == 0 {
        Vec::new()
    } else {
        vec![AppliedOperation {
            index: 0,
            operation: "replace".into(),
        }]
    };
    let outcome = service::save_mutation(context, &document, replaced, operations, args.dry_run)
        .map_err(CliFailure::from)?;
    let human = if truncated {
        format!(
            "{}; {} replacement(s), stopped at the configured limit",
            mutation_human(&outcome),
            count
        )
    } else {
        format!("{}; {} replacement(s)", mutation_human(&outcome), count)
    };
    let output = CommandOutput::new(
        "document.replace",
        json!({
            "outcome": outcome,
            "replacementCount": count,
            "truncated": truncated
        }),
        human,
    )?;
    if truncated {
        Ok(output
            .warning(format!(
                "Replacement stopped after {} matches because --max-replacements was reached.",
                args.max_replacements
            ))
            .partial())
    } else {
        Ok(output)
    }
}

fn document_edit(
    args: DocumentEditArgs,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    let document = service::read_document(context, &args.path).map_err(CliFailure::from)?;
    let request: DocumentEditRequestV1 = read_json(context, &args.request)?;
    if request.schema_version != 1 {
        return Err(CliFailure::new(
            "unsupported_schema",
            "Document edit schemaVersion must be 1.",
            2,
        ));
    }
    if let Some(expected) = request.expected_revision.as_ref() {
        let expected = cli_revision_argument(expected, "document.edit")?;
        let actual = document
            .revision
            .as_ref()
            .ok_or_else(|| {
                CliFailure::new(
                    "revision_conflict",
                    "The document no longer has a disk revision.",
                    4,
                )
                .for_command("document.edit")
            })?
            .to_disk_revision()
            .map_err(CliFailure::from)?;
        if actual != expected {
            return Err(CliFailure::new(
                "revision_conflict",
                "The document revision has changed.",
                4,
            )
            .for_command("document.edit"));
        }
    }
    let (content, applied) =
        apply_operations(&document.content, &request.operations).map_err(CliFailure::from)?;
    let outcome = service::save_mutation(context, &document, content, applied, args.dry_run)
        .map_err(CliFailure::from)?;
    let human = mutation_human(&outcome);
    CommandOutput::new("document.edit", outcome, human)
}

fn document_write(
    args: DocumentWriteArgs,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    let content = read_utf8(context, &args.input)?;
    let bom = if args.bom {
        Some(true)
    } else if args.no_bom {
        Some(false)
    } else {
        None
    };
    let outcome = service::write_document(
        context,
        &args.path,
        WriteOptions {
            content: &content,
            expected_hash: args.expected_hash.as_deref(),
            force: args.force,
            create: args.create,
            encoding: args.encoding.as_deref(),
            eol: args.eol.as_deref(),
            bom,
            dry_run: args.dry_run,
        },
    )
    .map_err(CliFailure::from)?;
    let human = mutation_human(&outcome);
    CommandOutput::new("document.write", outcome, human)
}

fn workspace_command(
    command: WorkspaceCommand,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    match command {
        WorkspaceCommand::Tree(args) => {
            let (store, root) = open_workspace(context, &args.workspace_root)?;
            let snapshot = store.open(&root).map_err(CliFailure::from)?;
            let human = snapshot
                .entries
                .iter()
                .map(|entry| format!("{}{}", "  ".repeat(entry.depth as usize), entry.name))
                .collect::<Vec<_>>()
                .join("\n");
            let items = snapshot
                .entries
                .iter()
                .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
                .collect();
            CommandOutput::new("workspace.tree", snapshot, human).map(|output| output.stream(items))
        }
        WorkspaceCommand::Search(args) => workspace_search_command(args, context, None),
        WorkspaceCommand::Create(args) => {
            let (store, root) = open_workspace(context, &args.workspace_root)?;
            let parent = workspace_child(context, &root, &args.parent)?;
            let target = store
                .preview_create_entry(&parent, &args.name)
                .map_err(CliFailure::from)?;
            if args.dry_run {
                return CommandOutput::new(
                    "workspace.create",
                    json!({ "dryRun": true, "path": target, "isDir": args.directory }),
                    format!("Would create {}", target.display()),
                );
            }
            let snapshot = store
                .create_entry_guarded(&parent, &args.name, args.directory, |target| {
                    let destination = context.capture_destination(target)?;
                    context.revalidate_destination(&destination)
                })
                .map_err(CliFailure::from)?;
            CommandOutput::new("workspace.create", snapshot, "Workspace entry created")
        }
        WorkspaceCommand::Rename(args) => {
            let (store, root) = open_workspace(context, &args.workspace_root)?;
            let path = workspace_child(context, &root, &args.path)?;
            let (_, target) = store
                .preview_rename_entry(&path, &args.new_name)
                .map_err(CliFailure::from)?;
            if args.dry_run {
                return CommandOutput::new(
                    "workspace.rename",
                    json!({ "dryRun": true, "path": path, "target": target }),
                    format!("Would rename {} to {}", path.display(), target.display()),
                );
            }
            let snapshot = store
                .rename_entry_guarded(&path, &args.new_name, |_, target| {
                    let destination = context.capture_destination(target)?;
                    context.revalidate_destination(&destination)
                })
                .map_err(CliFailure::from)?;
            CommandOutput::new("workspace.rename", snapshot, "Workspace entry renamed")
        }
        WorkspaceCommand::Trash(args) => {
            let (store, root) = open_workspace(context, &args.workspace_root)?;
            let path = workspace_child(context, &root, &args.path)?;
            let target = store.preview_trash_entry(&path).map_err(CliFailure::from)?;
            if args.dry_run {
                return CommandOutput::new(
                    "workspace.trash",
                    json!({ "dryRun": true, "path": target }),
                    format!("Would move {} to the Windows Recycle Bin", target.display()),
                );
            }
            require_yes(args.yes, "workspace.trash")?;
            let snapshot = store
                .trash_entry_guarded(&path, |target| {
                    let destination = context.capture_destination(target)?;
                    context.revalidate_destination(&destination)
                })
                .map_err(CliFailure::from)?;
            CommandOutput::new(
                "workspace.trash",
                snapshot,
                "Workspace entry moved to the Recycle Bin",
            )
        }
    }
}

fn workspace_search_command(
    args: WorkspaceSearchArgs,
    context: &CliContext,
    item_sender: Option<&mpsc::Sender<Value>>,
) -> Result<CommandOutput, CliFailure> {
    let (store, root) = open_workspace(context, &args.workspace_root)?;
    let request = SearchRequest {
        root: root.to_string_lossy().into_owned(),
        query: args.query,
        case_sensitive: args.case_sensitive,
        limit: Some(args.limit),
    };
    if let Some(item_sender) = item_sender {
        let count = store
            .search_with_control(
                request,
                |hit| {
                    let item = serde_json::to_value(hit)
                        .map_err(|error| ApiError::new("serialization_error", error.to_string()))?;
                    item_sender.send(item).map_err(|_| {
                        ApiError::new("broken_pipe", "The output consumer closed the pipe.")
                    })
                },
                cancelled,
            )
            .map_err(CliFailure::from)?;
        return CommandOutput::new("workspace.search", json!({ "count": count }), String::new())
            .map(|output| output.already_streamed(count));
    }

    let mut hits = Vec::new();
    store
        .search_with_control(
            request,
            |hit| {
                hits.push(hit.clone());
                Ok(())
            },
            cancelled,
        )
        .map_err(CliFailure::from)?;
    let human = hits
        .iter()
        .map(|hit| {
            format!(
                "{}:{}:{}: {}",
                hit.relative_path, hit.line, hit.column, hit.preview
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let items = hits
        .iter()
        .map(|hit| serde_json::to_value(hit).unwrap_or(Value::Null))
        .collect();
    CommandOutput::new(
        "workspace.search",
        json!({ "hits": hits, "count": hits.len() }),
        human,
    )
    .map(|output| output.stream(items))
}

fn asset_command(command: AssetCommand, context: &CliContext) -> Result<CommandOutput, CliFailure> {
    match command {
        AssetCommand::Add(args) => {
            if args.source.is_none() && !args.stdin {
                return Err(CliFailure::new(
                    "missing_asset",
                    "Provide --source or --stdin.",
                    2,
                ));
            }
            let data_base64 = if args.stdin {
                let bytes = read_stdin_bytes(Some(MAX_STDIN_ASSET_BYTES + 1))?;
                if bytes.len() as u64 > MAX_STDIN_ASSET_BYTES {
                    return Err(CliFailure::new(
                        "asset_too_large",
                        "Images cannot exceed 50MB.",
                        3,
                    )
                    .for_command("asset.add"));
                }
                Some(STANDARD.encode(bytes))
            } else {
                None
            };

            let pending_insertion =
                if let (Some(document_path), Some(line)) = (args.document.as_deref(), args.line) {
                    let document =
                        service::read_document(context, document_path).map_err(CliFailure::from)?;
                    let position = TextPosition {
                        line: line as usize,
                        column: args.column as usize,
                    };
                    // Validate the user-controlled position before writing the
                    // asset so an invalid line/column cannot leave an orphan.
                    validate_position(&document.content, position).map_err(CliFailure::from)?;
                    Some((document, position))
                } else {
                    None
                };

            let asset_operation = if args.dry_run {
                service::preview_asset
            } else {
                service::add_asset
            };
            let result = asset_operation(
                context,
                args.document.as_deref(),
                args.document_id.as_deref(),
                args.source.as_deref(),
                data_base64,
                Some(args.mime_type),
            )
            .map_err(CliFailure::from)?;

            if args.dry_run {
                let mutation = if let Some((document, position)) = pending_insertion {
                    let operation = DocumentEditOperation::Replace {
                        range: TextRange {
                            start: position,
                            end: position,
                        },
                        expected_text: String::new(),
                        text: markdown_image_link(&args.alt, &result.markdown_path),
                    };
                    let (content, applied) = apply_operations(&document.content, &[operation])
                        .map_err(CliFailure::from)?;
                    Some(
                        service::save_mutation(context, &document, content, applied, true)
                            .map_err(CliFailure::from)?,
                    )
                } else {
                    None
                };
                let human = if mutation.is_some() {
                    format!(
                        "Would write {} and insert its Markdown link",
                        result.absolute_path
                    )
                } else {
                    format!("Would write {}", result.absolute_path)
                };
                return CommandOutput::new(
                    "asset.add",
                    json!({ "dryRun": true, "asset": result, "documentMutation": mutation }),
                    human,
                );
            }

            let mut output =
                CommandOutput::new("asset.add", &result, result.markdown_path.clone())?;
            if let Some((document, position)) = pending_insertion {
                let operation = DocumentEditOperation::Replace {
                    range: TextRange {
                        start: position,
                        end: position,
                    },
                    expected_text: String::new(),
                    text: markdown_image_link(&args.alt, &result.markdown_path),
                };
                let insertion = apply_operations(&document.content, &[operation]).and_then(
                    |(content, applied)| {
                        service::save_mutation(context, &document, content, applied, false)
                    },
                );
                if let Err(error) = insertion {
                    output = output
                        .warning(format!(
                            "The asset was written but its Markdown link was not inserted: {}",
                            error.message
                        ))
                        .partial();
                }
            }
            Ok(output)
        }
        AssetCommand::Read(args) => {
            let document = context
                .existing_file(&args.document)
                .map_err(CliFailure::from)?;
            let workspace = args
                .workspace
                .as_deref()
                .map(|path| context.existing_path(path))
                .transpose()
                .map_err(CliFailure::from)?;
            let resource_scope = workspace.as_deref().or(context.root.as_deref());
            let data_url = asset::read_resource(&document, resource_scope, &args.resource)
                .map_err(CliFailure::from)?;
            CommandOutput::new("asset.read", json!({ "dataUrl": data_url }), data_url)
        }
    }
}

fn recovery_command(
    command: RecoveryCommand,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    let recovery = context.recovery().map_err(CliFailure::from)?;
    match command {
        RecoveryCommand::List => {
            let mut entries = recovery.list().map_err(CliFailure::from)?;
            let original_count = entries.len();
            if context.root.is_some() {
                entries
                    .retain(|entry| recovery_path_in_scope(context, entry.path.as_deref()).is_ok());
            }
            let excluded = original_count - entries.len();
            let human = entries
                .iter()
                .map(|entry| format!("{}  {}  {}", entry.id, entry.created_at, entry.title))
                .collect::<Vec<_>>()
                .join("\n");
            let items = entries
                .iter()
                .map(|entry| serde_json::to_value(entry).unwrap_or(Value::Null))
                .collect();
            let output = CommandOutput::new(
                "recovery.list",
                json!({ "entries": entries, "count": entries.len() }),
                human,
            )?;
            let output = if excluded > 0 {
                output.warning(format!(
                    "{excluded} recovery snapshot(s) outside --root were omitted."
                ))
            } else {
                output
            };
            Ok(output.stream(items))
        }
        RecoveryCommand::Checkpoint(args) => {
            let document = service::read_document(context, &args.path).map_err(CliFailure::from)?;
            let id = args.document_id.unwrap_or_else(|| {
                blake3::hash(document.path.as_deref().unwrap_or_default().as_bytes()).to_hex()[..32]
                    .to_string()
            });
            let entry = recovery
                .checkpoint(CheckpointRequest {
                    document_id: id,
                    path: document.path,
                    title: document.title,
                    content: document.content,
                    kind: Some(args.kind),
                })
                .map_err(CliFailure::from)?;
            CommandOutput::new(
                "recovery.checkpoint",
                json!({ "entry": entry }),
                if entry.is_some() {
                    "Recovery checkpoint created"
                } else {
                    "No new checkpoint was needed"
                },
            )
        }
        RecoveryCommand::Restore(args) => {
            let snapshot = recovery.restore(&args.id).map_err(CliFailure::from)?;
            recovery_path_in_scope(context, snapshot.entry.path.as_deref())?;
            if let Some(output_path) = args.output {
                let outcome = service::write_document(
                    context,
                    &output_path,
                    WriteOptions {
                        content: &snapshot.content,
                        expected_hash: args.expected_hash.as_deref(),
                        force: args.force,
                        create: args.create,
                        encoding: None,
                        eol: None,
                        bom: None,
                        dry_run: false,
                    },
                )
                .map_err(CliFailure::from)?;
                let human = mutation_human(&outcome);
                CommandOutput::new(
                    "recovery.restore",
                    json!({ "snapshot": snapshot.entry, "outcome": outcome }),
                    human,
                )
            } else {
                let human = snapshot.content.clone();
                CommandOutput::new("recovery.restore", snapshot, human)
            }
        }
        RecoveryCommand::Delete(args) => {
            require_yes(args.yes, "recovery.delete")?;
            if context.root.is_some() {
                let snapshot = recovery.restore(&args.id).map_err(CliFailure::from)?;
                recovery_path_in_scope(context, snapshot.entry.path.as_deref())?;
            }
            let deleted = recovery.delete(&args.id).map_err(CliFailure::from)?;
            if !deleted {
                return Err(CliFailure::new(
                    "recovery_not_found",
                    "The requested recovery snapshot no longer exists.",
                    3,
                ));
            }
            CommandOutput::new(
                "recovery.delete",
                json!({ "id": args.id, "deleted": true }),
                "Recovery snapshot deleted",
            )
        }
    }
}

fn markdown_image_link(alt: &str, path: &str) -> String {
    let mut escaped = String::with_capacity(alt.len());
    let mut characters = alt.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\r' => {
                if characters.peek() == Some(&'\n') {
                    characters.next();
                }
                escaped.push(' ');
            }
            '\n' => escaped.push(' '),
            '\\' | '[' | ']' => {
                escaped.push('\\');
                escaped.push(character);
            }
            character if character.is_control() => escaped.push(' '),
            character => escaped.push(character),
        }
    }
    let mut destination = String::with_capacity(path.len());
    for character in path.chars() {
        if character.is_control() {
            let mut encoded = [0u8; 4];
            for byte in character.encode_utf8(&mut encoded).as_bytes() {
                destination.push_str(&format!("%{byte:02X}"));
            }
        } else {
            if matches!(character, '\\' | '<' | '>') {
                destination.push('\\');
            }
            destination.push(character);
        }
    }
    format!("![{escaped}](<{destination}>)")
}

fn settings_command(
    command: SettingsCommand,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    let path = context.data_dir.join("settings.json");
    let store = SettingsStore::load(path);
    match command {
        SettingsCommand::Get => {
            let settings = store.snapshot().map_err(CliFailure::from)?;
            let (settings, excluded) = scoped_settings_output(context, settings);
            let output = CommandOutput::new("settings.get", settings, "InkFlow settings")?;
            Ok(with_scoped_path_warning(output, excluded))
        }
        SettingsCommand::Patch(args) => {
            let mut patch: SettingsPatchV1 = read_json(context, &args.input)?;
            normalize_settings_patch_paths(context, &mut patch)?;
            let dropped_scoped_paths = std::cell::Cell::new((0usize, 0usize));
            let settings = store
                .update_latest(|settings| {
                    let (updated, dropped) =
                        apply_settings_patch_for_context(context, settings.clone(), patch);
                    dropped_scoped_paths.set(dropped);
                    *settings = updated;
                })
                .map_err(CliFailure::from)?;
            let (settings, excluded) = scoped_settings_output(context, settings);
            let mut output =
                CommandOutput::new("settings.patch", settings, "InkFlow settings updated")?;
            let (dropped_files, dropped_workspaces) = dropped_scoped_paths.get();
            if dropped_files > 0 {
                output = output.warning(format!(
                    "{dropped_files} requested in-root recent file(s) were not saved because preserved out-of-root entries use the 20-file recent history capacity."
                ));
            }
            if dropped_workspaces > 0 {
                output = output.warning(format!(
                    "{dropped_workspaces} requested in-root recent workspace(s) were not saved because preserved out-of-root entries use the 10-workspace recent history capacity."
                ));
            }
            if dropped_files > 0 || dropped_workspaces > 0 {
                output = output.partial();
            }
            Ok(with_scoped_path_warning(output, excluded))
        }
        SettingsCommand::Reset => {
            let settings = store.reset().map_err(CliFailure::from)?;
            CommandOutput::new("settings.reset", settings, "InkFlow settings reset")
        }
    }
}

fn session_command(
    command: SessionCommand,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    let path = context.data_dir.join("session.json");
    let store = SessionStore::load(path.clone());
    match command {
        SessionCommand::Get => {
            let (session, revision) = store.snapshot().map_err(CliFailure::from)?;
            let revision = revision.map(CliDiskRevision::from);
            let (session, excluded) = scoped_session_output(context, session);
            let output = CommandOutput::new(
                "session.get",
                json!({ "session": session, "revision": revision }),
                "InkFlow session",
            )?;
            Ok(with_scoped_path_warning(output, excluded))
        }
        SessionCommand::Update(args) => {
            let guard = verify_state_hash(&path, args.expected_hash.as_deref(), args.force)?;
            let mut session: SessionV1 = read_json(context, &args.input)?;
            if session.schema_version != 1 {
                return Err(CliFailure::new(
                    "unsupported_schema",
                    "Session schemaVersion must be 1.",
                    2,
                )
                .for_command("session.update"));
            }
            normalize_session_paths(context, &mut session)?;
            let dropped_scoped_tabs = std::cell::Cell::new(0usize);
            let (session, revision) = if context.root.is_some() {
                store.update_scoped_guarded(
                    (!args.force)
                        .then_some(guard.expected_revision.as_ref())
                        .flatten(),
                    !args.force && guard.must_not_exist,
                    |current| {
                        let (merged, dropped) = merge_scoped_session(context, current, session);
                        dropped_scoped_tabs.set(dropped);
                        merged
                    },
                )
            } else {
                store.update_guarded_snapshot(
                    session,
                    (!args.force)
                        .then_some(guard.expected_revision.as_ref())
                        .flatten(),
                    !args.force && guard.must_not_exist,
                )
            }
            .map_err(CliFailure::from)?;
            let revision = CliDiskRevision::from(revision);
            let (session, excluded) = scoped_session_output(context, session);
            let mut output = CommandOutput::new(
                "session.update",
                json!({ "session": session, "revision": revision }),
                "InkFlow session updated",
            )?
            .warning(
                "If the desktop app is running, its later session save may supersede this value.",
            );
            let dropped_scoped_tabs = dropped_scoped_tabs.get();
            if dropped_scoped_tabs > 0 {
                output = output
                    .warning(format!(
                        "{dropped_scoped_tabs} requested in-root session tab(s) were not saved because preserved out-of-root tabs use the {MAX_SESSION_TABS}-tab session capacity."
                    ))
                    .partial();
            }
            Ok(with_scoped_path_warning(output, excluded))
        }
        SessionCommand::Clear(args) => {
            require_yes(args.yes, "session.clear")?;
            let session = if context.root.is_some() {
                store
                    .update_scoped_guarded(None, false, |current| {
                        merge_scoped_session(context, current, SessionV1::default()).0
                    })
                    .map(|(session, _)| session)
            } else {
                store.update(SessionV1::default())
            }
            .map_err(CliFailure::from)?;
            let (session, excluded) = scoped_session_output(context, session);
            let output = CommandOutput::new("session.clear", session, "InkFlow session cleared")?;
            Ok(with_scoped_path_warning(output, excluded))
        }
    }
}

fn render_command(
    command: RenderCommand,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    match command {
        RenderCommand::Fragment(args) => {
            let document = read_source(context, &args.source.source)?;
            let output_target = args
                .output
                .as_deref()
                .map(|output| context.destination_path(output))
                .transpose()
                .map_err(CliFailure::from)?;
            let output_guard = output_target
                .as_ref()
                .map(|output| {
                    verify_output_target(context, output, None, args.force, "render.fragment")
                })
                .transpose()?;
            let explicit_document_path = args
                .document_path
                .as_deref()
                .map(|path| context.existing_file(path))
                .transpose()
                .map_err(CliFailure::from)?;
            let document_path = explicit_document_path
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .or_else(|| document.path.clone());
            let data = renderer_client::execute(
                context,
                renderer_client::RendererRequest {
                    operation: "fragment",
                    title: &document.title,
                    markdown: &document.content,
                    document_path: document_path.as_deref(),
                    output_path: None,
                    output_destination: None,
                    expected_output_revision: None,
                    output_must_not_exist: false,
                    output_must_exist: false,
                    allow_remote_images: args.allow_remote_images,
                    page_size: None,
                    landscape: None,
                },
            )
            .map_err(CliFailure::from)?;
            let html = data
                .get("html")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    CliFailure::new(
                        "invalid_renderer_response",
                        "The renderer did not return HTML.",
                        3,
                    )
                })?
                .to_string();
            if let Some(output) = output_target {
                let output_guard = output_guard
                    .as_ref()
                    .expect("an output target always has a validated guard");
                export::write_export_bytes_validated(
                    &output,
                    html.as_bytes(),
                    Some(&output_guard.as_export_guard()),
                    || context.revalidate_destination(&output_guard.destination),
                )
                .map_err(CliFailure::from)?;
                CommandOutput::new(
                    "render.fragment",
                    json!({ "path": output, "renderer": "webview2" }),
                    format!("Rendered {}", output.display()),
                )
            } else {
                CommandOutput::new(
                    "render.fragment",
                    json!({ "html": html, "renderer": "webview2" }),
                    html,
                )
            }
        }
    }
}

fn export_command(
    command: ExportCommand,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    match command {
        ExportCommand::Html(args) => export_with_renderer("html", args, context),
        ExportCommand::Pdf(args) => export_with_renderer("pdf", args, context),
    }
}

fn export_with_renderer(
    operation: &str,
    args: ExportArgs,
    context: &CliContext,
) -> Result<CommandOutput, CliFailure> {
    let document = service::read_document(context, &args.source).map_err(CliFailure::from)?;
    let output = context
        .destination_path(&args.output)
        .map_err(CliFailure::from)?;
    let output_guard = verify_output_target(
        context,
        &output,
        args.expected_output_hash.as_deref(),
        args.force,
        &format!("export.{operation}"),
    )?;
    let title = args.title.unwrap_or_else(|| document.title.clone());
    let output_string = output.to_string_lossy().into_owned();
    let data = renderer_client::execute(
        context,
        renderer_client::RendererRequest {
            operation,
            title: &title,
            markdown: &document.content,
            document_path: document.path.as_deref(),
            output_path: Some(&output_string),
            output_destination: Some(&output_guard.destination),
            expected_output_revision: output_guard.write.expected_revision.as_ref(),
            output_must_not_exist: output_guard.write.must_not_exist,
            output_must_exist: output_guard.write.must_exist,
            allow_remote_images: args.allow_remote_images,
            page_size: Some(&args.page_size),
            landscape: Some(args.landscape),
        },
    )
    .map_err(CliFailure::from)?;
    CommandOutput::new(
        format!("export.{operation}"),
        json!({ "outcome": data.get("outcome").cloned().unwrap_or(data), "renderer": "webview2" }),
        format!("Exported {}", output.display()),
    )
}

struct OutputTargetGuard {
    expected_revision: Option<crate::model::DiskRevision>,
    must_not_exist: bool,
    must_exist: bool,
}

struct ExportTargetGuard {
    destination: service::DestinationSnapshot,
    write: OutputTargetGuard,
}

impl ExportTargetGuard {
    fn as_export_guard(&self) -> export::ExportWriteGuard {
        export::ExportWriteGuard {
            expected_revision: self.write.expected_revision.clone(),
            create_only: self.write.must_not_exist,
            require_existing: self.write.must_exist,
        }
    }
}

fn verify_output_target(
    context: &CliContext,
    path: &Path,
    expected_hash: Option<&str>,
    force: bool,
    command: &str,
) -> Result<ExportTargetGuard, CliFailure> {
    let (destination, captured_revision) = context
        .capture_file_destination(path)
        .map_err(CliFailure::from)?;
    if destination.path() != path {
        return Err(CliFailure::new(
            "path_changed",
            "The export destination path changed while it was being prepared.",
            3,
        )
        .for_command(command));
    }
    let Some(revision) = captured_revision else {
        if expected_hash.is_some() {
            return Err(CliFailure::new(
                "revision_conflict",
                "The expected export destination no longer exists.",
                4,
            )
            .for_command(command));
        }
        return Ok(ExportTargetGuard {
            destination,
            write: OutputTargetGuard {
                expected_revision: None,
                must_not_exist: true,
                must_exist: false,
            },
        });
    };
    if let Some(expected) = expected_hash {
        if revision.hash != expected {
            return Err(CliFailure::new(
                "revision_conflict",
                "The export destination hash has changed.",
                4,
            )
            .for_command(command));
        }
        return Ok(ExportTargetGuard {
            destination,
            write: OutputTargetGuard {
                expected_revision: Some(revision),
                must_not_exist: false,
                must_exist: true,
            },
        });
    }
    if force {
        Ok(ExportTargetGuard {
            destination,
            write: OutputTargetGuard {
                expected_revision: None,
                must_not_exist: false,
                must_exist: true,
            },
        })
    } else {
        Err(CliFailure::new(
            "confirmation_required",
            "Replacing an existing output requires --expected-output-hash or --force.",
            5,
        )
        .for_command(command))
    }
}

fn app_command(command: AppCommand, context: &CliContext) -> Result<CommandOutput, CliFailure> {
    match command {
        AppCommand::Open(args) => {
            let executable = desktop_executable()?;
            let workspace = if let Some(workspace) = args.workspace {
                let workspace = context
                    .existing_path(&workspace)
                    .map_err(CliFailure::from)?;
                if !workspace.is_dir() {
                    return Err(CliFailure::new(
                        "not_a_directory",
                        "--workspace must identify a directory.",
                        3,
                    ));
                }
                Some(workspace)
            } else {
                None
            };
            let paths = args
                .paths
                .into_iter()
                .map(|path| context.existing_file(&path).map_err(CliFailure::from))
                .collect::<Result<Vec<_>, _>>()?;
            let arguments = desktop_open_arguments(workspace, paths);
            let pid = launch_desktop(&executable, &arguments)?;
            CommandOutput::new(
                "app.open",
                json!({ "pid": pid, "executable": executable }),
                format!("InkFlow started (PID {pid})"),
            )
        }
    }
}

fn desktop_open_arguments(workspace: Option<PathBuf>, paths: Vec<PathBuf>) -> Vec<OsString> {
    let mut arguments = Vec::with_capacity(paths.len() + usize::from(workspace.is_some()) * 2);
    if let Some(workspace) = workspace {
        arguments.push(OsString::from(crate::DESKTOP_OPEN_WORKSPACE_FLAG));
        arguments.push(workspace.into_os_string());
    }
    arguments.extend(paths.into_iter().map(PathBuf::into_os_string));
    arguments
}

#[cfg(windows)]
fn launch_desktop(executable: &Path, arguments: &[OsString]) -> Result<u32, CliFailure> {
    use std::os::windows::ffi::OsStrExt;

    use windows::{
        Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION,
                STARTUPINFOW,
            },
        },
        core::{PCWSTR, PWSTR},
    };

    let application = executable
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut command_line = Vec::new();
    append_windows_argument(&mut command_line, executable.as_os_str());
    for argument in arguments {
        append_windows_argument(&mut command_line, argument);
    }
    command_line.push(0);
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    let launch = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command_line.as_mut_ptr())),
            None,
            None,
            false,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            None,
            PCWSTR::null(),
            &startup,
            &mut process,
        )
    };
    launch.map_err(|error| CliFailure::new("app_launch_error", error.to_string(), 3))?;
    let pid = process.dwProcessId;
    unsafe {
        let _ = CloseHandle(process.hThread);
        let _ = CloseHandle(process.hProcess);
    }
    if pid == 0 {
        Err(CliFailure::new(
            "app_launch_error",
            "Windows created the desktop process without a process identifier.",
            3,
        ))
    } else {
        Ok(pid)
    }
}

#[cfg(windows)]
fn append_windows_argument(command_line: &mut Vec<u16>, value: &OsStr) {
    use std::os::windows::ffi::OsStrExt;

    if !command_line.is_empty() {
        command_line.push(' ' as u16);
    }
    let value = value.encode_wide().collect::<Vec<_>>();
    let quoted = value.is_empty()
        || value
            .iter()
            .any(|unit| *unit == b' ' as u16 || *unit == b'\t' as u16 || *unit == b'"' as u16);
    if !quoted {
        command_line.extend(value);
        return;
    }

    command_line.push(b'"' as u16);
    let mut backslashes = 0usize;
    for unit in value {
        if unit == b'\\' as u16 {
            backslashes += 1;
        } else if unit == b'"' as u16 {
            command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2 + 1));
            command_line.push(unit);
            backslashes = 0;
        } else {
            command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes));
            command_line.push(unit);
            backslashes = 0;
        }
    }
    command_line.extend(std::iter::repeat_n(b'\\' as u16, backslashes * 2));
    command_line.push(b'"' as u16);
}

#[cfg(not(windows))]
fn launch_desktop(executable: &Path, arguments: &[OsString]) -> Result<u32, CliFailure> {
    let child = ProcessCommand::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| CliFailure::new("app_launch_error", error.to_string(), 3))?;
    Ok(child.id())
}

fn read_source(context: &CliContext, source: &Path) -> Result<model::DocumentInfo, CliFailure> {
    if source == Path::new("-") {
        return Ok(service::stdin_document(read_stdin_utf8()?));
    }
    service::read_document(context, source).map_err(CliFailure::from)
}

fn read_utf8(context: &CliContext, path: &Path) -> Result<String, CliFailure> {
    if path == Path::new("-") {
        read_stdin_utf8()
    } else {
        let path = context.existing_path(path).map_err(CliFailure::from)?;
        fs::read_to_string(path)
            .map_err(|error| CliFailure::new("input_error", error.to_string(), 3))
    }
}

fn read_stdin_utf8() -> Result<String, CliFailure> {
    String::from_utf8(read_stdin_bytes(None)?)
        .map_err(|error| CliFailure::new("stdin_error", error.to_string(), 3))
}

fn read_stdin_bytes(limit: Option<u64>) -> Result<Vec<u8>, CliFailure> {
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = if let Some(limit) = limit {
            io::stdin().take(limit).read_to_end(&mut bytes)
        } else {
            io::stdin().read_to_end(&mut bytes)
        }
        .map(|_| bytes);
        let _ = sender.send(result);
    });

    loop {
        match receiver.recv_timeout(CANCELLATION_POLL_INTERVAL) {
            Ok(Ok(bytes)) => return Ok(bytes),
            Ok(Err(error)) => {
                return Err(CliFailure::new("stdin_error", error.to_string(), 3));
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(CliFailure::new(
                    "stdin_error",
                    "The stdin reader stopped without returning a result.",
                    3,
                ));
            }
            Err(RecvTimeoutError::Timeout) if cancelled() => {
                return Err(cancelled_failure("inkflow-cli"));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
}

fn read_json<T: serde::de::DeserializeOwned>(
    context: &CliContext,
    path: &Path,
) -> Result<T, CliFailure> {
    let value = read_utf8(context, path)?;
    serde_json::from_str(&value)
        .map_err(|error| CliFailure::new("invalid_json", error.to_string(), 2))
}

fn open_workspace(
    context: &CliContext,
    input: &Path,
) -> Result<(WorkspaceStore, PathBuf), CliFailure> {
    let unresolved = service::resolve_from_current_directory(input).map_err(CliFailure::from)?;
    service::reject_reparse_path(&unresolved).map_err(CliFailure::from)?;
    let root = context.existing_path(input).map_err(CliFailure::from)?;
    let store = WorkspaceStore::new();
    store.select_root(&root).map_err(CliFailure::from)?;
    Ok((store, root))
}

fn workspace_child(context: &CliContext, root: &Path, input: &Path) -> Result<PathBuf, CliFailure> {
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    service::reject_reparse_path(&candidate).map_err(CliFailure::from)?;
    let resolved = if candidate.exists() {
        context.existing_path(&candidate)
    } else {
        let parent = candidate.parent().unwrap_or(root);
        context
            .existing_path(parent)
            .map(|parent| parent.join(candidate.file_name().unwrap_or_default()))
    }
    .map_err(CliFailure::from)?;
    fileio::ensure_within(root, &resolved).map_err(CliFailure::from)
}

fn require_yes(value: bool, command: &str) -> Result<(), CliFailure> {
    if value {
        Ok(())
    } else {
        Err(CliFailure::new(
            "confirmation_required",
            format!("{command} requires --yes; InkFlow CLI never prompts interactively."),
            5,
        )
        .for_command(command))
    }
}

fn cli_revision_argument(
    revision: &CliDiskRevision,
    command: &str,
) -> Result<crate::model::DiskRevision, CliFailure> {
    revision
        .to_disk_revision()
        .map_err(|error| CliFailure::new(error.code, error.message, 2).for_command(command))
}

fn verify_state_hash(
    path: &Path,
    expected: Option<&str>,
    force: bool,
) -> Result<OutputTargetGuard, CliFailure> {
    if !path.exists() {
        if expected.is_some() {
            return Err(CliFailure::new(
                "revision_conflict",
                "The expected state file no longer exists.",
                4,
            ));
        }
        return Ok(OutputTargetGuard {
            expected_revision: None,
            must_not_exist: true,
            must_exist: false,
        });
    }
    if force {
        return Ok(OutputTargetGuard {
            expected_revision: None,
            must_not_exist: false,
            must_exist: true,
        });
    }
    let expected = expected.ok_or_else(|| {
        CliFailure::new(
            "confirmation_required",
            "Updating an existing state file requires --expected-hash or --force.",
            5,
        )
    })?;
    let current = fileio::revision(path).map_err(CliFailure::from)?;
    if current.hash != expected {
        return Err(CliFailure::new(
            "revision_conflict",
            "The state file hash has changed.",
            4,
        ));
    }
    Ok(OutputTargetGuard {
        expected_revision: Some(current),
        must_not_exist: false,
        must_exist: true,
    })
}

fn normalize_settings_patch_paths(
    context: &CliContext,
    patch: &mut SettingsPatchV1,
) -> Result<(), CliFailure> {
    if let Some(paths) = patch.recent_files.as_mut() {
        normalize_path_list(context, paths)?;
    }
    if let Some(paths) = patch.recent_workspaces.as_mut() {
        normalize_path_list(context, paths)?;
    }
    Ok(())
}

fn normalize_session_paths(
    context: &CliContext,
    session: &mut SessionV1,
) -> Result<(), CliFailure> {
    if let Some(workspace) = session.workspace_root.as_mut() {
        *workspace = normalized_configuration_path(context, workspace)?;
    }
    for tab in &mut session.tabs {
        tab.path = normalized_configuration_path(context, &tab.path)?;
    }
    if let Some(active) = session.active_path.as_mut() {
        *active = normalized_configuration_path(context, active)?;
    }
    Ok(())
}

fn normalize_path_list(context: &CliContext, paths: &mut [String]) -> Result<(), CliFailure> {
    for path in paths {
        *path = normalized_configuration_path(context, path)?;
    }
    Ok(())
}

fn normalized_configuration_path(context: &CliContext, path: &str) -> Result<String, CliFailure> {
    if path.trim().is_empty() {
        return Err(CliFailure::new(
            "invalid_path",
            "Configured paths cannot be empty.",
            2,
        ));
    }
    context
        .destination_path(Path::new(path))
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(CliFailure::from)
}

fn scoped_settings_output(context: &CliContext, mut settings: SettingsV1) -> (SettingsV1, usize) {
    if context.root.is_none() {
        return (settings, 0);
    }
    let (recent_files, file_excluded) = scoped_path_output(context, settings.recent_files);
    let (recent_workspaces, workspace_excluded) =
        scoped_path_output(context, settings.recent_workspaces);
    settings.recent_files = recent_files;
    settings.recent_workspaces = recent_workspaces;
    (settings, file_excluded + workspace_excluded)
}

fn scoped_session_output(context: &CliContext, mut session: SessionV1) -> (SessionV1, usize) {
    if context.root.is_none() {
        return (session, 0);
    }
    let mut excluded = 0;
    session.workspace_root = session.workspace_root.and_then(|path| {
        normalized_configuration_path(context, &path)
            .map_err(|_| excluded += 1)
            .ok()
    });
    session.tabs.retain_mut(
        |tab| match normalized_configuration_path(context, &tab.path) {
            Ok(path) => {
                tab.path = path;
                true
            }
            Err(_) => {
                excluded += 1;
                false
            }
        },
    );
    session.active_path = session.active_path.and_then(|path| {
        let normalized = normalized_configuration_path(context, &path)
            .map_err(|_| excluded += 1)
            .ok()?;
        session
            .tabs
            .iter()
            .any(|tab| paths_equal(&tab.path, &normalized))
            .then_some(normalized)
    });
    if session.active_path.is_none() {
        session.active_path = session.tabs.first().map(|tab| tab.path.clone());
    }
    (session, excluded)
}

fn merge_scoped_session(
    context: &CliContext,
    mut current: SessionV1,
    scoped: SessionV1,
) -> (SessionV1, usize) {
    let hidden_limit = current
        .tabs
        .iter()
        .filter(|tab| !configuration_path_is_in_scope(context, &tab.path))
        .count()
        .min(MAX_SESSION_TABS);
    let requested_scoped_tabs = scoped.tabs.len().min(MAX_SESSION_TABS);
    let scoped_capacity = MAX_SESSION_TABS.saturating_sub(hidden_limit);
    let dropped_scoped_tabs = requested_scoped_tabs.saturating_sub(scoped_capacity);
    let mut scoped_tabs = Some(
        scoped
            .tabs
            .into_iter()
            .take(scoped_capacity)
            .collect::<Vec<_>>(),
    );
    let mut tabs = Vec::with_capacity(MAX_SESSION_TABS);
    let mut hidden = 0;
    for tab in current.tabs {
        if configuration_path_is_in_scope(context, &tab.path) {
            if let Some(replacement) = scoped_tabs.take() {
                tabs.extend(replacement);
            }
        } else if hidden < hidden_limit {
            tabs.push(tab);
            hidden += 1;
        }
    }
    if let Some(replacement) = scoped_tabs.take() {
        tabs.extend(replacement);
    }
    current.tabs = tabs;

    let current_workspace_is_scoped = current
        .workspace_root
        .as_deref()
        .is_some_and(|path| configuration_path_is_in_scope(context, path));
    if current.workspace_root.is_none() || current_workspace_is_scoped {
        current.workspace_root = scoped.workspace_root;
    }

    let current_active_is_scoped = current
        .active_path
        .as_deref()
        .is_some_and(|path| configuration_path_is_in_scope(context, path));
    if current.active_path.is_none() || current_active_is_scoped {
        current.active_path = scoped.active_path;
    }
    (current, dropped_scoped_tabs)
}

fn configuration_path_is_in_scope(context: &CliContext, path: &str) -> bool {
    normalized_configuration_path(context, path).is_ok()
}

fn scoped_path_output(context: &CliContext, paths: Vec<String>) -> (Vec<String>, usize) {
    let original_count = paths.len();
    let paths = paths
        .into_iter()
        .filter_map(|path| normalized_configuration_path(context, &path).ok())
        .collect::<Vec<_>>();
    let excluded = original_count - paths.len();
    (paths, excluded)
}

fn paths_equal(left: &str, right: &str) -> bool {
    left.replace('/', "\\")
        .eq_ignore_ascii_case(&right.replace('/', "\\"))
}

fn with_scoped_path_warning(output: CommandOutput, excluded: usize) -> CommandOutput {
    if excluded == 0 {
        output
    } else {
        output.warning(format!(
            "{excluded} configured path(s) outside or inaccessible through --root were omitted."
        ))
    }
}

fn recovery_path_in_scope(context: &CliContext, path: Option<&str>) -> Result<(), CliFailure> {
    if context.root.is_none() {
        return Ok(());
    }
    let path = path.ok_or_else(|| {
        CliFailure::new(
            "path_outside_workspace",
            "An unsaved recovery snapshot cannot be accessed through --root.",
            3,
        )
    })?;
    context
        .scoped_path_allow_missing(Path::new(path))
        .map(|_| ())
        .map_err(CliFailure::from)
}

fn apply_settings_patch(mut settings: SettingsV1, patch: SettingsPatchV1) -> SettingsV1 {
    if let Some(value) = patch.locale {
        settings.locale = value;
    }
    if let Some(value) = patch.theme {
        settings.theme = value;
    }
    if let Some(value) = patch.page_width {
        settings.page_width = value;
    }
    if let Some(value) = patch.font_size {
        settings.font_size = value;
    }
    if let Some(value) = patch.line_height {
        settings.line_height = value;
    }
    if let Some(value) = patch.editor_font {
        settings.editor_font = value;
    }
    if let Some(value) = patch.code_font {
        settings.code_font = value;
    }
    if let Some(value) = patch.autosave_delay_ms {
        settings.autosave_delay_ms = value;
    }
    if let Some(value) = patch.show_file_tree {
        settings.show_file_tree = value;
    }
    if let Some(value) = patch.show_outline {
        settings.show_outline = value;
    }
    if let Some(value) = patch.focus_mode {
        settings.focus_mode = value;
    }
    if let Some(value) = patch.typewriter_mode {
        settings.typewriter_mode = value;
    }
    if let Some(value) = patch.recent_files {
        settings.recent_files = value;
    }
    if let Some(value) = patch.recent_workspaces {
        settings.recent_workspaces = value;
    }
    settings
}

fn apply_settings_patch_for_context(
    context: &CliContext,
    mut settings: SettingsV1,
    mut patch: SettingsPatchV1,
) -> (SettingsV1, (usize, usize)) {
    let mut dropped_files = 0;
    let mut dropped_workspaces = 0;
    if context.root.is_some() {
        if let Some(replacement) = patch.recent_files.take() {
            let (merged, dropped) =
                merge_scoped_path_list(context, settings.recent_files, replacement, 20);
            settings.recent_files = merged;
            dropped_files = dropped;
        }
        if let Some(replacement) = patch.recent_workspaces.take() {
            let (merged, dropped) =
                merge_scoped_path_list(context, settings.recent_workspaces, replacement, 10);
            settings.recent_workspaces = merged;
            dropped_workspaces = dropped;
        }
    }
    (
        apply_settings_patch(settings, patch),
        (dropped_files, dropped_workspaces),
    )
}

fn merge_scoped_path_list(
    context: &CliContext,
    current: Vec<String>,
    replacement: Vec<String>,
    limit: usize,
) -> (Vec<String>, usize) {
    let hidden_limit = current
        .iter()
        .filter(|path| !configuration_path_is_in_scope(context, path))
        .count()
        .min(limit);
    let requested_scoped_paths = replacement.len().min(limit);
    let scoped_capacity = limit.saturating_sub(hidden_limit);
    let dropped_scoped_paths = requested_scoped_paths.saturating_sub(scoped_capacity);
    let mut replacement = Some(
        replacement
            .into_iter()
            .take(scoped_capacity)
            .collect::<Vec<_>>(),
    );
    let mut merged = Vec::with_capacity(limit);
    let mut hidden = 0;
    for path in current {
        if configuration_path_is_in_scope(context, &path) {
            if let Some(replacement) = replacement.take() {
                merged.extend(replacement);
            }
        } else if hidden < hidden_limit {
            merged.push(path);
            hidden += 1;
        }
    }
    if let Some(replacement) = replacement {
        merged.extend(replacement);
    }
    (merged, dropped_scoped_paths)
}

fn desktop_executable() -> Result<PathBuf, CliFailure> {
    if let Some(value) = std::env::var_os("INKFLOW_DESKTOP_EXE") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(CliFailure::new(
            "app_not_found",
            "INKFLOW_DESKTOP_EXE is not a file.",
            3,
        ));
    }
    let current = std::env::current_exe()
        .map_err(|error| CliFailure::new("app_not_found", error.to_string(), 3))?;
    let sibling = current.with_file_name("InkFlow.exe");
    if sibling.is_file() {
        return Ok(sibling);
    }
    let lowercase = current.with_file_name("inkflow.exe");
    if lowercase.is_file() {
        return Ok(lowercase);
    }
    Err(CliFailure::new(
        "app_not_found",
        "InkFlow.exe was not found beside inkflow-cli.exe; set INKFLOW_DESKTOP_EXE for development.",
        3,
    ))
}

fn mutation_human(outcome: &model::DocumentMutationOutcome) -> String {
    if !outcome.changed {
        format!("No changes: {}", outcome.path)
    } else if outcome.dry_run {
        outcome.diff.clone().unwrap_or_else(|| {
            format!(
                "Would update {} (new content hash {})",
                outcome.path, outcome.content_hash
            )
        })
    } else {
        format!("Updated {}", outcome.path)
    }
}

fn format_from_raw_args(args: &[String]) -> OutputFormat {
    for (index, value) in args.iter().enumerate() {
        if let Some(format) = value.strip_prefix("--format=") {
            return parse_output_format(format);
        }
        if value == "--format" {
            if let Some(format) = args.get(index + 1) {
                return parse_output_format(format);
            }
        }
    }
    OutputFormat::Auto
}

fn parse_output_format(value: &str) -> OutputFormat {
    match value.to_ascii_lowercase().as_str() {
        "text" => OutputFormat::Text,
        "json" => OutputFormat::Json,
        "jsonl" => OutputFormat::Jsonl,
        _ => OutputFormat::Auto,
    }
}

fn resolve_format(format: OutputFormat) -> OutputFormat {
    if format == OutputFormat::Auto {
        if io::stdout().is_terminal() {
            OutputFormat::Text
        } else {
            OutputFormat::Json
        }
    } else {
        format
    }
}

fn emit_success(output: CommandOutput, format: OutputFormat) -> io::Result<()> {
    match format {
        OutputFormat::Text | OutputFormat::Auto => {
            let mut stdout = io::stdout().lock();
            if !output.human.is_empty() {
                writeln!(stdout, "{}", output.human)?;
            }
            stdout.flush()?;
            let mut stderr = io::stderr().lock();
            for warning in output.warnings {
                writeln!(stderr, "warning: {warning}")?;
            }
            stderr.flush()
        }
        OutputFormat::Json => {
            let envelope = CliEnvelope {
                api_version: CLI_API_VERSION,
                ok: true,
                command: output.command,
                data: output.data,
                warnings: output.warnings,
            };
            let mut stdout = io::stdout().lock();
            write_json_line(&mut stdout, &envelope)?;
            stdout.flush()
        }
        OutputFormat::Jsonl => {
            let mut stdout = io::stdout().lock();
            if let Some(count) = output.streamed_count {
                write_json_line(
                    &mut stdout,
                    &json!({
                        "apiVersion": CLI_API_VERSION,
                        "type": "summary",
                        "command": output.command,
                        "count": count,
                        "warnings": output.warnings
                    }),
                )?;
            } else if let Some(items) = output.stream_items {
                let count = items.len();
                for item in items {
                    write_json_line(
                        &mut stdout,
                        &json!({
                            "apiVersion": CLI_API_VERSION,
                            "type": "item",
                            "command": output.command,
                            "data": item
                        }),
                    )?;
                }
                write_json_line(
                    &mut stdout,
                    &json!({
                        "apiVersion": CLI_API_VERSION,
                        "type": "summary",
                        "command": output.command,
                        "count": count,
                        "warnings": output.warnings
                    }),
                )?;
            } else {
                write_json_line(
                    &mut stdout,
                    &json!({
                        "apiVersion": CLI_API_VERSION,
                        "type": "result",
                        "command": output.command,
                        "data": output.data,
                        "warnings": output.warnings
                    }),
                )?;
            }
            stdout.flush()
        }
    }
}

fn emit_jsonl_item(command: &str, item: &impl Serialize) -> io::Result<()> {
    let value = json!({
        "apiVersion": CLI_API_VERSION,
        "type": "item",
        "command": command,
        "data": item
    });
    let mut stdout = io::stdout().lock();
    write_json_line(&mut stdout, &value)?;
    stdout.flush()
}

fn emit_pending_jsonl_items(receiver: &mpsc::Receiver<Value>) -> io::Result<()> {
    for item in receiver.try_iter() {
        emit_jsonl_item("workspace.search", &item)?;
    }
    Ok(())
}

fn write_json_line(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *writer, value).map_err(|error| {
        if let Some(kind) = error.io_error_kind() {
            io::Error::new(kind, error)
        } else {
            io::Error::other(error)
        }
    })?;
    writer.write_all(b"\n")
}

fn emitted_exit(result: io::Result<()>, intended_exit: i32) -> i32 {
    match result {
        Ok(()) => intended_exit,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "output_error: {error}");
            3
        }
    }
}

fn emit_failure(failure: &CliFailure, format: OutputFormat) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    write_failure(failure, format, &mut stdout, &mut stderr)
}

fn write_failure(
    failure: &CliFailure,
    format: OutputFormat,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<()> {
    match format {
        OutputFormat::Text | OutputFormat::Auto => {
            writeln!(stderr, "{}: {}", failure.error.code, failure.error.message)?;
            stderr.flush()
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            let envelope = CliErrorEnvelope {
                api_version: CLI_API_VERSION,
                ok: false,
                command: failure.command.clone(),
                error: failure.error.clone(),
            };
            write_json_line(stdout, &envelope)?;
            stdout.flush()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_auto_resolution_can_be_overridden() {
        assert_eq!(
            format_from_raw_args(&["inkflow-cli".into(), "--format=jsonl".into()]),
            OutputFormat::Jsonl
        );
        assert_eq!(
            format_from_raw_args(&["inkflow-cli".into(), "--format".into(), "text".into()]),
            OutputFormat::Text
        );
    }

    #[test]
    fn settings_patch_only_changes_present_fields() {
        let original = SettingsV1::default();
        let updated = apply_settings_patch(
            original.clone(),
            SettingsPatchV1 {
                theme: Some("dark".into()),
                ..Default::default()
            },
        );
        assert_eq!(updated.theme, "dark");
        assert_eq!(updated.font_size, original.font_size);
    }

    #[test]
    fn desktop_open_arguments_mark_the_workspace_before_document_paths() {
        let workspace = PathBuf::from(r"C:\Notes");
        let document = PathBuf::from(r"C:\Notes\note.md");
        let arguments = desktop_open_arguments(Some(workspace.clone()), vec![document.clone()]);

        assert_eq!(
            arguments,
            vec![
                OsString::from(crate::DESKTOP_OPEN_WORKSPACE_FLAG),
                workspace.into_os_string(),
                document.into_os_string(),
            ]
        );
    }

    #[test]
    fn scoped_settings_path_merge_reserves_space_for_hidden_entries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        std::fs::create_dir(&root).unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), Some(root.clone())).unwrap();
        let outside = temp
            .path()
            .join("outside.md")
            .to_string_lossy()
            .into_owned();
        let current = std::iter::once(root.join("old.md").to_string_lossy().into_owned())
            .chain(std::iter::once(outside.clone()))
            .collect();
        let replacement = (0..20)
            .map(|index| {
                root.join(format!("new-{index}.md"))
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        let (merged, dropped) = merge_scoped_path_list(&context, current, replacement, 20);

        assert_eq!(merged.len(), 20);
        assert_eq!(dropped, 1);
        assert!(merged.contains(&outside));
        assert!(!merged.iter().any(|path| path.ends_with("old.md")));
    }

    #[test]
    fn scoped_session_merge_reserves_space_for_hidden_tabs() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside_root = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside_root).unwrap();
        let context = CliContext::new(Some(temp.path().join("data")), Some(root.clone())).unwrap();
        let tab = |path: PathBuf| crate::model::SessionTabV1 {
            path: path.to_string_lossy().into_owned(),
            mode: "live".into(),
        };
        let current = SessionV1 {
            tabs: std::iter::once(tab(root.join("old.md")))
                .chain((0..49).map(|index| tab(outside_root.join(format!("hidden-{index}.md")))))
                .collect(),
            ..SessionV1::default()
        };
        let scoped = SessionV1 {
            tabs: (0..MAX_SESSION_TABS)
                .map(|index| tab(root.join(format!("new-{index}.md"))))
                .collect(),
            ..SessionV1::default()
        };

        let (merged, dropped) = merge_scoped_session(&context, current, scoped);

        assert_eq!(merged.tabs.len(), MAX_SESSION_TABS);
        assert_eq!(dropped, 49);
        assert_eq!(
            merged
                .tabs
                .iter()
                .filter(|entry| Path::new(&entry.path).starts_with(&outside_root))
                .count(),
            49
        );
        assert_eq!(
            merged
                .tabs
                .iter()
                .filter(|entry| Path::new(&entry.path).starts_with(&root))
                .count(),
            1
        );
        assert!(
            !merged
                .tabs
                .iter()
                .any(|entry| entry.path.ends_with("old.md"))
        );
    }

    #[test]
    fn broken_output_pipe_is_a_clean_termination() {
        assert_eq!(
            emitted_exit(Err(io::Error::from(io::ErrorKind::BrokenPipe)), 3),
            0
        );
    }

    #[test]
    fn interruptible_execution_stops_waiting_for_non_cooperative_work() {
        let result = run_interruptibly(
            || {
                thread::sleep(Duration::from_millis(100));
                42
            },
            || true,
            Duration::from_millis(1),
            Duration::from_millis(5),
            CancellationPolicy::AbandonAfterGrace,
        );

        assert_eq!(result, Err(InterruptibleError::Cancelled));
    }

    #[test]
    fn interruptible_execution_allows_cooperative_cleanup_to_finish() {
        let result = run_interruptibly(
            || {
                thread::sleep(Duration::from_millis(5));
                42
            },
            || true,
            Duration::from_millis(1),
            Duration::from_millis(100),
            CancellationPolicy::AbandonAfterGrace,
        );

        assert_eq!(result, Ok(42));
    }

    #[test]
    fn interruptible_execution_waits_for_guarded_work_to_finish() {
        let result = run_interruptibly(
            || {
                thread::sleep(Duration::from_millis(25));
                42
            },
            || true,
            Duration::from_millis(1),
            Duration::from_millis(5),
            CancellationPolicy::FinishBeforeExit,
        );

        assert_eq!(result, Ok(42));
    }

    #[test]
    fn disk_mutations_use_safe_cancellation_policy() {
        let read = Cli::try_parse_from(["inkflow-cli", "document", "read", "-"]).unwrap();
        let save_as = Cli::try_parse_from([
            "inkflow-cli",
            "document",
            "save-as",
            "source.md",
            "destination.md",
        ])
        .unwrap();

        assert_eq!(
            cancellation_policy(&read.command),
            CancellationPolicy::AbandonAfterGrace
        );
        assert_eq!(
            cancellation_policy(&save_as.command),
            CancellationPolicy::FinishBeforeExit
        );
    }

    #[test]
    fn cancelled_json_output_uses_the_versioned_error_envelope() {
        let failure = match resolve_execution(
            Err(InterruptibleError::Cancelled),
            "document.read".to_string(),
        ) {
            Ok(_) => panic!("a cancelled execution must resolve to a failure"),
            Err(failure) => failure,
        };
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        write_failure(&failure, OutputFormat::Json, &mut stdout, &mut stderr).unwrap();

        assert_eq!(String::from_utf8_lossy(&stdout).lines().count(), 1);
        let envelope: Value = serde_json::from_slice(&stdout).unwrap();
        assert_eq!(envelope["apiVersion"], CLI_API_VERSION);
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["command"], "document.read");
        assert_eq!(envelope["error"]["code"], "cancelled");
        assert!(stderr.is_empty());
    }

    #[test]
    fn image_alt_text_cannot_break_out_of_the_markdown_label() {
        assert_eq!(
            markdown_image_link("diagram [v1]\\draft\r\nnext", "note.assets/image.png"),
            "![diagram \\[v1\\]\\\\draft next](<note.assets/image.png>)"
        );
    }

    #[test]
    fn image_paths_with_spaces_remain_valid_markdown_destinations() {
        use pulldown_cmark::{Event, Parser, Tag};

        let markdown = markdown_image_link("image", "My Notes.assets/image.png");

        assert_eq!(markdown, "![image](<My Notes.assets/image.png>)");
        assert!(Parser::new(&markdown).any(|event| {
            matches!(
                event,
                Event::Start(Tag::Image { dest_url, .. })
                    if dest_url.as_ref() == "My Notes.assets/image.png"
            )
        }));
    }
}
