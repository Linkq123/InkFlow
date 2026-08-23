#![cfg(feature = "cli")]

use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Output, Stdio},
};

use pulldown_cmark::{Event, Parser, Tag};
use serde_json::Value;

fn run(args: &[&str], stdin: Option<&str>) -> Output {
    run_with_env(args, stdin, &[])
}

fn run_with_env(args: &[&str], stdin: Option<&str>, environment: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_inkflow-cli"));
    command
        .args(args)
        .envs(environment.iter().copied())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().expect("start inkflow-cli");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    child.wait_with_output().expect("read CLI output")
}

fn parse(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON stdout ({error}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn assert_envelope(output: &Output, command: &str, ok: bool) -> Value {
    let json = parse(output);
    assert_eq!(json["apiVersion"], "inkflow.cli/v1", "{command}");
    assert_eq!(json["command"], command, "{command}");
    assert_eq!(json["ok"], ok, "{command}: {json}");
    json
}

#[test]
fn capabilities_use_the_versioned_envelope_without_stdout_logs() {
    let output = run(&["--format", "json", "capabilities"], None);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let json = parse(&output);
    assert_eq!(json["apiVersion"], "inkflow.cli/v1");
    assert_eq!(json["ok"], true);
    assert_eq!(json["command"], "capabilities");
    assert_eq!(
        json["data"]["commands"]["capabilities"],
        serde_json::json!([])
    );
    assert_eq!(json["data"]["commands"]["schema"], serde_json::json!([]));
    assert_eq!(json["data"]["limits"]["documentEditOperations"], 256);
    assert_eq!(json["data"]["limits"]["inlineFormatContextBytes"], 524_288);
}

#[test]
fn self_discovery_commands_do_not_require_a_writable_data_directory() {
    let temp = tempfile::tempdir().unwrap();
    let blocked_data_directory = temp.path().join("not-a-directory");
    std::fs::write(&blocked_data_directory, "block directory creation").unwrap();
    let blocked_data_directory = path(&blocked_data_directory);

    for command in ["capabilities", "schema"] {
        let output = run(
            &[
                "--format",
                "json",
                "--data-dir",
                &blocked_data_directory,
                command,
            ],
            None,
        );

        assert_eq!(output.status.code(), Some(0), "{command}");
        assert!(output.stderr.is_empty(), "{command}");
        assert_envelope(&output, command, true);
    }
}

#[test]
fn context_initialization_errors_keep_the_parsed_command_name() {
    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            "relative-data",
            "document",
            "read",
            "missing.md",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let json = assert_envelope(&output, "document.read", false);
    assert_eq!(json["error"]["code"], "invalid_data_directory");
}

#[test]
fn auto_format_uses_json_when_stdout_is_piped() {
    let output = run(&["capabilities"], None);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_envelope(&output, "capabilities", true);
}

#[cfg(windows)]
#[test]
fn app_open_detaches_standard_streams_from_a_long_running_desktop_child() {
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("fake InkFlow.vbs");
    std::fs::write(&script, "WScript.Sleep 5000\r\n").unwrap();
    let launcher = Path::new(&std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("cscript.exe");
    assert!(launcher.is_file());
    let launcher_arg = path(&launcher);
    let script_arg = path(&script);

    let started = std::time::Instant::now();
    let output = run_with_env(
        &["--format", "json", "app", "open", &script_arg],
        None,
        &[("INKFLOW_DESKTOP_EXE", &launcher_arg)],
    );
    let elapsed = started.elapsed();

    assert_eq!(output.status.code(), Some(0));
    let envelope = assert_envelope(&output, "app.open", true);
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "app open waited for the desktop child for {elapsed:?}"
    );
    let pid = envelope["data"]["pid"].as_u64().unwrap() as u32;
    unsafe {
        use windows::Win32::{
            Foundation::{CloseHandle, WAIT_TIMEOUT},
            System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        };

        let process = OpenProcess(PROCESS_SYNCHRONIZE, false, pid)
            .expect("desktop child should still exist after the CLI returns");
        assert_eq!(
            WaitForSingleObject(process, 0),
            WAIT_TIMEOUT,
            "desktop child exited before app open returned"
        );
        CloseHandle(process).unwrap();
    }
}

#[test]
fn invalid_arguments_return_exit_code_two_as_json() {
    let output = run(&["--format", "json", "not-a-command"], None);
    assert_eq!(output.status.code(), Some(2));
    let json = parse(&output);
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "invalid_arguments");
}

#[test]
fn invalid_document_encoding_returns_exit_code_two() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let output = run(
        &[
            "--format",
            "json",
            "document",
            "write",
            &path(&document),
            "--create",
            "--encoding",
            "definitely-not-an-encoding",
        ],
        Some("content"),
    );

    assert_eq!(output.status.code(), Some(2));
    let json = assert_envelope(&output, "document.write", false);
    assert_eq!(json["error"]["code"], "unsupported_encoding");
    assert!(!document.exists());
}

#[test]
fn literal_document_replace_rejects_an_empty_query_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let data = temp.path().join("data");
    std::fs::write(&document, "abc\n").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "document",
            "replace",
            &path(&document),
            "",
            "X",
            "--all",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(2));
    let json = assert_envelope(&output, "document.replace", false);
    assert_eq!(json["error"]["code"], "invalid_query");
    assert_eq!(std::fs::read_to_string(document).unwrap(), "abc\n");
}

#[test]
fn literal_document_search_rejects_an_empty_query() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let data = temp.path().join("data");
    std::fs::write(&document, "abc\n").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "document",
            "search",
            &path(&document),
            "",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(2));
    let json = assert_envelope(&output, "document.search", false);
    assert_eq!(json["error"]["code"], "invalid_query");
}

#[test]
fn zero_width_document_search_includes_empty_logical_lines() {
    let temp = tempfile::tempdir().unwrap();
    let empty = temp.path().join("empty.md");
    let trailing = temp.path().join("trailing.md");
    let data = temp.path().join("data");
    std::fs::write(&empty, "").unwrap();
    std::fs::write(&trailing, "content\n").unwrap();

    for (document, expected_line) in [(&empty, 1), (&trailing, 2)] {
        let output = run(
            &[
                "--format",
                "json",
                "--data-dir",
                &path(&data),
                "document",
                "search",
                &path(document),
                "^$",
                "--regex",
            ],
            None,
        );

        assert_eq!(output.status.code(), Some(0));
        let json = assert_envelope(&output, "document.search", true);
        assert_eq!(json["data"]["count"], 1);
        assert_eq!(json["data"]["hits"][0]["line"], expected_line);
        assert_eq!(json["data"]["hits"][0]["column"], 1);
        assert_eq!(json["data"]["hits"][0]["endColumn"], 1);
    }
}

#[test]
fn capped_document_replace_reports_partial_success() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let data = temp.path().join("data");
    let original = "x ".repeat(4);
    std::fs::write(&document, &original).unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "document",
            "replace",
            &path(&document),
            "x",
            "y",
            "--all",
            "--max-replacements",
            "2",
            "--dry-run",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(6));
    let json = assert_envelope(&output, "document.replace", true);
    assert_eq!(json["data"]["replacementCount"], 2);
    assert_eq!(json["data"]["truncated"], true);
    assert_eq!(json["warnings"].as_array().unwrap().len(), 1);
    assert_eq!(std::fs::read_to_string(document).unwrap(), original);
}

#[test]
fn schema_exposes_session_update_payloads() {
    let output = run(&["--format", "json", "schema"], None);
    assert_eq!(output.status.code(), Some(0));
    let schema = parse(&output);
    let definitions = schema["data"]["$defs"].as_object().unwrap();
    assert!(definitions.contains_key("SessionV1"));
    assert!(definitions.contains_key("SessionTabV1"));
    assert_eq!(
        definitions["CliEnvelope"]["properties"]["apiVersion"]["const"],
        "inkflow.cli/v1"
    );
    assert_eq!(
        definitions["CliEnvelope"]["properties"]["ok"]["const"],
        true
    );
    assert_eq!(
        definitions["CliErrorEnvelope"]["properties"]["ok"]["const"],
        false
    );
    assert_eq!(
        definitions["DocumentEditRequestV1"]["properties"]["schemaVersion"]["const"],
        1
    );
    assert_eq!(
        definitions["DocumentEditRequestV1"]["properties"]["operations"]["maxItems"],
        256
    );
    assert_eq!(
        definitions["TextPosition"]["properties"]["line"]["minimum"],
        1
    );
    assert_eq!(
        definitions["TextPosition"]["properties"]["column"]["minimum"],
        1
    );
    assert_eq!(
        definitions["SettingsPatchV1"]["properties"]["theme"]["enum"],
        serde_json::json!([null, "system", "light", "dark"])
    );
    assert_eq!(definitions["SessionV1"]["additionalProperties"], false);
    assert_eq!(definitions["SessionTabV1"]["additionalProperties"], false);
    assert_eq!(
        definitions["SessionV1"]["properties"]["schemaVersion"]["const"],
        1
    );
    assert_eq!(
        definitions["CliDiskRevision"]["properties"]["modifiedAt"]["format"],
        "date-time"
    );
}

#[test]
fn session_update_rejects_unknown_fields_and_unsupported_versions() {
    for request in [
        r#"{"schemaVersion":1,"workspaceRoot":null,"tabs":[],"activePath":null,"extra":true}"#,
        r#"{"schemaVersion":1,"workspaceRoot":null,"tabs":[{"path":"note.md","mode":"live","extra":true}],"activePath":null}"#,
        r#"{"schemaVersion":2,"workspaceRoot":null,"tabs":[],"activePath":null}"#,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let output = run(
            &[
                "--format",
                "json",
                "--data-dir",
                &path(&data),
                "session",
                "update",
                "--force",
            ],
            Some(request),
        );

        assert_eq!(output.status.code(), Some(2));
        let json = assert_envelope(&output, "session.update", false);
        assert!(matches!(
            json["error"]["code"].as_str(),
            Some("invalid_json" | "unsupported_schema")
        ));
        assert!(!data.join("session.json").exists());
    }
}

#[test]
fn guarded_document_write_round_trips_and_refuses_unguarded_overwrite() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("中文 note.md");
    let data = temp.path().join("data");
    let document_arg = path(&document);
    let data_arg = path(&data);
    let create = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "document",
            "write",
            &document_arg,
            "--create",
            "--eol",
            "crlf",
            "--input",
            "-",
        ],
        Some("# 标题\r\n\r\nEmoji 😀\r\n"),
    );
    assert_eq!(create.status.code(), Some(0));
    assert_eq!(parse(&create)["ok"], true);

    let read = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "document",
            "read",
            &document_arg,
        ],
        None,
    );
    assert_eq!(read.status.code(), Some(0));
    let read = parse(&read);
    assert_eq!(read["data"]["content"], "# 标题\n\nEmoji 😀\n");
    let modified_at = read["data"]["revision"]["modifiedAt"]
        .as_str()
        .expect("revision exposes modifiedAt");
    assert!(modified_at.ends_with('Z'));
    chrono::DateTime::parse_from_rfc3339(modified_at).expect("valid RFC 3339 revision time");
    assert!(read["data"]["revision"].get("modifiedMs").is_none());

    let overwrite = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "document",
            "write",
            &document_arg,
            "--input",
            "-",
        ],
        Some("unsafe"),
    );
    assert_eq!(overwrite.status.code(), Some(5));
    assert_eq!(parse(&overwrite)["command"], "document.write");
    assert_eq!(
        std::fs::read_to_string(document).unwrap(),
        "# 标题\r\n\r\nEmoji 😀\r\n"
    );
}

#[test]
fn guarded_document_write_rejects_an_expected_hash_when_the_target_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("missing.md");

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "write",
            &path(&document),
            "--create",
            "--expected-hash",
            "stale",
        ],
        Some("must not be written"),
    );

    assert_eq!(output.status.code(), Some(4));
    let envelope = assert_envelope(&output, "document.write", false);
    assert_eq!(envelope["error"]["code"], "revision_conflict");
    assert!(!document.exists());
}

#[test]
fn new_utf16_document_defaults_to_a_bom_and_round_trips() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("utf16.md");
    let document_arg = path(&document);
    let create = run(
        &[
            "--format",
            "json",
            "document",
            "write",
            &document_arg,
            "--create",
            "--encoding",
            "utf-16le",
        ],
        Some("# 标题\n\nEmoji 😀\n"),
    );
    assert_eq!(create.status.code(), Some(0));
    assert!(std::fs::read(&document).unwrap().starts_with(&[0xff, 0xfe]));

    let read = run(
        &["--format", "json", "document", "read", &document_arg],
        None,
    );
    assert_eq!(read.status.code(), Some(0));
    let json = assert_envelope(&read, "document.read", true);
    assert_eq!(json["data"]["encoding"], "utf-16le");
    assert_eq!(json["data"]["hadBom"], true);
    assert_eq!(json["data"]["content"], "# 标题\n\nEmoji 😀\n");
}

#[test]
fn utf16_without_a_bom_is_rejected_before_creating_a_file() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("utf16-no-bom.md");
    let output = run(
        &[
            "--format",
            "json",
            "document",
            "write",
            &path(&document),
            "--create",
            "--encoding",
            "utf-16le",
            "--no-bom",
        ],
        Some("# 标题\n"),
    );

    assert_eq!(output.status.code(), Some(2));
    let json = assert_envelope(&output, "document.write", false);
    assert_eq!(json["error"]["code"], "invalid_bom");
    assert!(!document.exists());
}

#[test]
fn bom_is_rejected_for_legacy_encodings() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("legacy.md");
    let output = run(
        &[
            "--format",
            "json",
            "document",
            "write",
            &path(&document),
            "--create",
            "--encoding",
            "windows-1252",
            "--bom",
        ],
        Some("plain text"),
    );

    assert_eq!(output.status.code(), Some(2));
    let json = assert_envelope(&output, "document.write", false);
    assert_eq!(json["error"]["code"], "invalid_bom");
    assert!(!document.exists());
}

#[test]
fn root_restriction_rejects_an_outside_document() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let output = run(
        &[
            "--format",
            "json",
            "--root",
            &path(root.path()),
            "document",
            "read",
            &path(outside.path()),
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(parse(&output)["command"], "document.read");
}

#[test]
fn global_root_restricts_workspace_positionals() {
    let temp = tempfile::tempdir().unwrap();
    let allowed = temp.path().join("allowed");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&allowed).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("outside.md"), "outside").unwrap();
    let allowed_arg = path(&allowed);
    let outside_arg = path(&outside);

    let tree = run(
        &[
            "--format",
            "json",
            "--root",
            &allowed_arg,
            "workspace",
            "tree",
            &outside_arg,
        ],
        None,
    );
    assert_eq!(tree.status.code(), Some(3));
    assert_eq!(parse(&tree)["error"]["code"], "path_outside_workspace");

    let create = run(
        &[
            "--format",
            "json",
            "--root",
            &allowed_arg,
            "workspace",
            "create",
            &outside_arg,
            ".",
            "escaped.md",
            "--dry-run",
        ],
        None,
    );
    assert_eq!(create.status.code(), Some(3));
    assert_eq!(parse(&create)["error"]["code"], "path_outside_workspace");
    assert!(!outside.join("escaped.md").exists());
}

#[test]
fn expected_text_mismatch_is_a_revision_conflict_and_does_not_write() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let request = temp.path().join("edit.json");
    std::fs::write(&document, "current text\n").unwrap();
    std::fs::write(
        &request,
        r#"{"schemaVersion":1,"expectedRevision":null,"operations":[{"type":"replace","range":{"start":{"line":1,"column":1},"end":{"line":1,"column":8}},"expectedText":"stale!!","text":"changed"}]}"#,
    )
    .unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "edit",
            &path(&document),
            "--request",
            &path(&request),
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    assert_eq!(parse(&output)["error"]["code"], "expected_text_mismatch");
    assert_eq!(std::fs::read_to_string(document).unwrap(), "current text\n");
}

#[test]
fn edit_request_rejects_too_many_operations_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let original = "- [ ] task\n";
    std::fs::write(&document, original).unwrap();
    let operations = (0..=256)
        .map(|_| serde_json::json!({ "type": "toggleTask", "line": 1, "checked": true }))
        .collect::<Vec<_>>();
    let request = serde_json::json!({
        "schemaVersion": 1,
        "expectedRevision": null,
        "operations": operations
    })
    .to_string();

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "edit",
            &path(&document),
            "--request",
            "-",
        ],
        Some(&request),
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(parse(&output)["error"]["code"], "too_many_operations");
    assert_eq!(std::fs::read_to_string(document).unwrap(), original);
}

#[test]
fn task_toggle_rejects_indented_code_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("code.md");
    let original = "    - [ ] sample code\n";
    std::fs::write(&document, original).unwrap();
    let request = serde_json::json!({
        "schemaVersion": 1,
        "expectedRevision": null,
        "operations": [{ "type": "toggleTask", "line": 1, "checked": true }]
    })
    .to_string();

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "edit",
            &path(&document),
            "--request",
            "-",
        ],
        Some(&request),
    );

    assert_eq!(output.status.code(), Some(3));
    let envelope = assert_envelope(&output, "document.edit", false);
    assert_eq!(envelope["error"]["code"], "not_a_task");
    assert_eq!(std::fs::read_to_string(document).unwrap(), original);
}

#[test]
fn edit_request_rejects_unknown_safety_fields() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    std::fs::write(&document, "current text\n").unwrap();
    let request = r#"{"schemaVersion":1,"expectedRevison":null,"operations":[]}"#;

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "edit",
            &path(&document),
            "--request",
            "-",
        ],
        Some(request),
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(parse(&output)["error"]["code"], "invalid_json");
    assert_eq!(std::fs::read_to_string(document).unwrap(), "current text\n");
}

#[test]
fn edit_request_rejects_a_non_rfc3339_revision_time() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let document_arg = path(&document);
    std::fs::write(&document, "current text\n").unwrap();
    let read = run(
        &["--format", "json", "document", "read", &document_arg],
        None,
    );
    let mut revision = parse(&read)["data"]["revision"].clone();
    revision["modifiedAt"] = Value::String("not-a-time".into());
    let request = serde_json::json!({
        "schemaVersion": 1,
        "expectedRevision": revision,
        "operations": []
    })
    .to_string();

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "edit",
            &document_arg,
            "--request",
            "-",
        ],
        Some(&request),
    );

    assert_eq!(output.status.code(), Some(2));
    let json = assert_envelope(&output, "document.edit", false);
    assert_eq!(json["error"]["code"], "invalid_revision");
    assert_eq!(std::fs::read_to_string(document).unwrap(), "current text\n");
}

#[test]
fn workspace_search_jsonl_streams_items_then_a_summary() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("one.md"), "InkFlow first\n").unwrap();
    std::fs::write(temp.path().join("two.md"), "InkFlow second\n").unwrap();

    let output = run(
        &[
            "--format",
            "jsonl",
            "workspace",
            "search",
            &path(temp.path()),
            "InkFlow",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    let lines = String::from_utf8(output.stdout).unwrap();
    let values = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 3);
    assert!(values[..2].iter().all(|value| value["type"] == "item"));
    assert_eq!(values[2]["type"], "summary");
    assert_eq!(values[2]["count"], 2);
}

#[test]
fn workspace_search_reports_case_insensitive_columns_from_the_original_line() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("unicode.md"), "İx\n").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "workspace",
            "search",
            &path(temp.path()),
            "x",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    let envelope = assert_envelope(&output, "workspace.search", true);
    assert_eq!(envelope["data"]["hits"][0]["column"], 2);
}

#[test]
fn save_as_rejects_an_expected_hash_when_the_destination_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.md");
    let destination = temp.path().join("missing.md");
    std::fs::write(&source, "source").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "save-as",
            &path(&source),
            &path(&destination),
            "--expected-destination-hash",
            "stale",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(4));
    assert_eq!(parse(&output)["error"]["code"], "revision_conflict");
    assert!(!destination.exists());
}

#[test]
fn save_as_encoding_failure_does_not_leave_copied_assets() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.md");
    let source_assets = temp.path().join("source.assets");
    let destination = temp.path().join("中文.md");
    std::fs::create_dir(&source_assets).unwrap();
    std::fs::write(source_assets.join("image.png"), b"image").unwrap();
    std::fs::write(
        &source,
        b"![image](source.assets/image.png)\nlegacy caf\xE9\n",
    )
    .unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "save-as",
            &path(&source),
            &path(&destination),
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(parse(&output)["error"]["code"], "encoding_loss");
    assert!(!destination.exists());
    assert!(!temp.path().join("中文.assets").exists());
}

#[test]
fn save_as_decodes_character_references_before_copying_assets() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.md");
    let destination = temp.path().join("copy.md");
    let asset = temp.path().join("a&b.png");
    std::fs::write(
        &source,
        concat!(
            "![inline](a&amp;b.png)\n",
            "![reference][asset]\n\n",
            "[asset]: a&#38;b.png\n",
            "<img src=\"a&#x26;b.png\" srcset=\"a&amp;b.png 2x\">\n",
        ),
    )
    .unwrap();
    std::fs::write(&asset, b"image").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "document",
            "save-as",
            &path(&source),
            &path(&destination),
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    assert_envelope(&output, "document.saveAs", true);
    assert_eq!(
        std::fs::read_to_string(destination).unwrap(),
        concat!(
            "![inline](copy.assets/a%26b.png)\n",
            "![reference][asset]\n\n",
            "[asset]: copy.assets/a%26b.png\n",
            "<img src=\"copy.assets/a%26b.png\" srcset=\"copy.assets/a%26b.png 2x\">\n",
        )
    );
    assert_eq!(
        std::fs::read(temp.path().join("copy.assets/a&b.png")).unwrap(),
        b"image"
    );
}

#[test]
fn save_as_dry_run_hash_matches_same_named_asset_commit() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let source = temp.path().join("source.md");
    let destination = temp.path().join("copy.md");
    let data = temp.path().join("data");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    std::fs::write(first.join("image.png"), b"first image").unwrap();
    std::fs::write(second.join("image.png"), b"second image").unwrap();
    std::fs::write(
        &source,
        "![first](first/image.png)\n![second](second/image.png)\n",
    )
    .unwrap();
    let data_arg = path(&data);
    let source_arg = path(&source);
    let destination_arg = path(&destination);

    let dry_run = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "document",
            "save-as",
            &source_arg,
            &destination_arg,
            "--dry-run",
        ],
        None,
    );
    assert_eq!(dry_run.status.code(), Some(0));
    let dry_run = assert_envelope(&dry_run, "document.saveAs", true);

    let committed = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "document",
            "save-as",
            &source_arg,
            &destination_arg,
        ],
        None,
    );
    assert_eq!(committed.status.code(), Some(0));
    let committed = assert_envelope(&committed, "document.saveAs", true);

    assert_eq!(
        dry_run["data"]["contentHash"],
        committed["data"]["contentHash"]
    );
    let markdown = std::fs::read_to_string(destination).unwrap();
    let links = markdown.lines().collect::<Vec<_>>();
    assert_eq!(links.len(), 2);
    assert_ne!(links[0], links[1]);
}

#[test]
fn save_as_overwrite_checkpoints_the_previous_destination() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source.md");
    let destination = temp.path().join("destination.md");
    let data = temp.path().join("data");
    std::fs::write(&source, "new content\n").unwrap();
    std::fs::write(&destination, "previous destination\n").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "document",
            "save-as",
            &path(&source),
            &path(&destination),
            "--force",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0));

    let list = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "recovery",
            "list",
        ],
        None,
    );
    assert_eq!(list.status.code(), Some(0));
    let list = parse(&list);
    let entry = list["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["path"] == path(&destination))
        .expect("destination history checkpoint");
    let restored = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "recovery",
            "restore",
            entry["id"].as_str().unwrap(),
        ],
        None,
    );
    assert_eq!(restored.status.code(), Some(0));
    assert_eq!(
        parse(&restored)["data"]["content"],
        "previous destination\n"
    );
}

#[test]
fn scoped_recovery_survives_a_missing_document_parent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let document_parent = root.join("removed").join("nested");
    let document = document_parent.join("note.md");
    let data = temp.path().join("data");
    std::fs::create_dir_all(&document_parent).unwrap();
    std::fs::write(&document, "recover after directory loss\n").unwrap();
    let root_arg = path(&root);
    let data_arg = path(&data);
    let document_arg = path(&document);

    let checkpoint = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "recovery",
            "checkpoint",
            &document_arg,
        ],
        None,
    );
    assert_eq!(checkpoint.status.code(), Some(0));
    let checkpoint = assert_envelope(&checkpoint, "recovery.checkpoint", true);
    let recovery_id = checkpoint["data"]["entry"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::remove_dir_all(root.join("removed")).unwrap();

    let list = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "recovery",
            "list",
        ],
        None,
    );
    assert_eq!(list.status.code(), Some(0));
    let list = assert_envelope(&list, "recovery.list", true);
    assert_eq!(list["data"]["count"], 1);
    assert_eq!(list["data"]["entries"][0]["id"], recovery_id);

    let restore = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "recovery",
            "restore",
            &recovery_id,
        ],
        None,
    );
    assert_eq!(restore.status.code(), Some(0));
    let restore = assert_envelope(&restore, "recovery.restore", true);
    assert_eq!(restore["data"]["content"], "recover after directory loss\n");

    let delete = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "recovery",
            "delete",
            &recovery_id,
            "--yes",
        ],
        None,
    );
    assert_eq!(delete.status.code(), Some(0));
    assert_envelope(&delete, "recovery.delete", true);
}

#[test]
fn asset_dry_run_validates_input_without_writing_an_asset() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let image = temp.path().join("image.png");
    std::fs::write(&document, "note").unwrap();
    std::fs::write(&image, b"image").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "asset",
            "add",
            "--document",
            &path(&document),
            "--source",
            &path(&image),
            "--dry-run",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    let envelope = assert_envelope(&output, "asset.add", true);
    assert_eq!(envelope["data"]["dryRun"], true);
    assert!(!temp.path().join("note.assets").exists());

    let missing = temp.path().join("missing.png");
    let invalid = run(
        &[
            "--format",
            "json",
            "asset",
            "add",
            "--document",
            &path(&document),
            "--source",
            &path(&missing),
            "--dry-run",
        ],
        None,
    );
    assert_eq!(invalid.status.code(), Some(3));
    assert_eq!(parse(&invalid)["ok"], false);
    assert!(!temp.path().join("note.assets").exists());
}

#[test]
fn asset_insertion_keeps_spaced_document_paths_parseable() {
    let temp = tempfile::tempdir().unwrap();
    let data_directory = path(&temp.path().join("data"));
    let document = temp.path().join("My Notes.md");
    let image = temp.path().join("image.png");
    std::fs::write(&document, "start").unwrap();
    std::fs::write(&image, b"image").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_directory,
            "asset",
            "add",
            "--document",
            &path(&document),
            "--source",
            &path(&image),
            "--line",
            "1",
            "--column",
            "1",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0));
    let envelope = assert_envelope(&output, "asset.add", true);
    let markdown_path = envelope["data"]["markdownPath"].as_str().unwrap();
    let content = std::fs::read_to_string(document).unwrap();
    assert!(Parser::new(&content).any(|event| {
        matches!(
            event,
            Event::Start(Tag::Image { dest_url, .. }) if dest_url.as_ref() == markdown_path
        )
    }));
}

#[test]
fn data_directory_is_canonicalized_before_paths_are_returned() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    let nested = data.join("nested");
    std::fs::create_dir_all(&nested).unwrap();
    let lexical_data = nested.join("..");

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&lexical_data),
            "asset",
            "add",
            "--document-id",
            "draft",
            "--stdin",
            "--dry-run",
        ],
        Some("png fixture"),
    );

    assert_eq!(output.status.code(), Some(0));
    let json = assert_envelope(&output, "asset.add", true);
    let absolute_path = Path::new(json["data"]["asset"]["absolutePath"].as_str().unwrap());
    let canonical_data = dunce::canonicalize(&data).unwrap();
    assert!(absolute_path.is_absolute());
    assert!(absolute_path.starts_with(canonical_data));
    assert!(
        !absolute_path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    );
}

#[test]
fn root_restriction_rejects_an_unbound_pending_asset() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "--root",
            &path(&root),
            "asset",
            "add",
            "--document-id",
            "draft",
            "--stdin",
        ],
        Some("png fixture"),
    );

    assert_eq!(output.status.code(), Some(3));
    let envelope = assert_envelope(&output, "asset.add", false);
    assert_eq!(envelope["error"]["code"], "path_outside_workspace");
    assert!(!data.join("Recovery/assets").exists());
}

#[test]
fn invalid_asset_insertion_position_does_not_leave_an_orphan() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let image = temp.path().join("image.png");
    std::fs::write(&document, "one line").unwrap();
    std::fs::write(&image, b"image").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "asset",
            "add",
            "--document",
            &path(&document),
            "--source",
            &path(&image),
            "--line",
            "999",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(parse(&output)["error"]["code"], "invalid_position");
    assert!(!temp.path().join("note.assets").exists());
}

#[cfg(target_os = "windows")]
#[test]
fn root_restriction_rejects_a_junction_used_as_the_derived_asset_directory() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let document = root.join("note.md");
    let image = root.join("image.png");
    let junction = root.join("note.assets");
    std::fs::write(&document, "note").unwrap();
    std::fs::write(&image, b"image").unwrap();
    let created = Command::new("cmd.exe")
        .args(["/c", "mklink", "/J", &path(&junction), &path(&outside)])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let output = run(
        &[
            "--format",
            "json",
            "--root",
            &path(&root),
            "asset",
            "add",
            "--document",
            &path(&document),
            "--source",
            &path(&image),
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(parse(&output)["error"]["code"], "reparse_point_blocked");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    std::fs::remove_dir(&junction).unwrap();
}

#[cfg(target_os = "windows")]
#[test]
fn asset_read_rejects_a_junction_inside_root_before_resolving_the_resource() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let actual = root.join("actual");
    let junction = root.join("linked");
    let data = temp.path().join("data");
    std::fs::create_dir_all(&actual).unwrap();
    let document = root.join("note.md");
    std::fs::write(&document, "![image](linked/image.png)").unwrap();
    std::fs::write(actual.join("image.png"), b"image").unwrap();
    let created = Command::new("cmd.exe")
        .args(["/c", "mklink", "/J", &path(&junction), &path(&actual)])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "--root",
            &path(&root),
            "asset",
            "read",
            "--document",
            &path(&document),
            "linked/image.png",
        ],
        None,
    );
    std::fs::remove_dir(&junction).unwrap();

    assert_eq!(output.status.code(), Some(3));
    let envelope = assert_envelope(&output, "asset.read", false);
    assert_eq!(envelope["error"]["code"], "reparse_point_blocked");
}

#[cfg(target_os = "windows")]
#[test]
fn workspace_read_commands_reject_a_junction_used_as_the_workspace_root() {
    let temp = tempfile::tempdir().unwrap();
    let outside = temp.path().join("outside");
    let junction = temp.path().join("workspace-link");
    let data = temp.path().join("data");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("note.md"), "find me").unwrap();
    let created = Command::new("cmd.exe")
        .args(["/c", "mklink", "/J", &path(&junction), &path(&outside)])
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let tree = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "workspace",
            "tree",
            &path(&junction),
        ],
        None,
    );
    let search = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "workspace",
            "search",
            &path(&junction),
            "find me",
        ],
        None,
    );
    std::fs::remove_dir(&junction).unwrap();

    for (output, command) in [(tree, "workspace.tree"), (search, "workspace.search")] {
        assert_eq!(output.status.code(), Some(3));
        let envelope = assert_envelope(&output, command, false);
        assert_eq!(envelope["error"]["code"], "reparse_point_blocked");
    }
}

#[test]
fn a_closed_stdout_pipe_does_not_panic_or_report_an_operation_failure() {
    let temp = tempfile::tempdir().unwrap();
    for index in 0..500 {
        std::fs::write(
            temp.path().join(format!("note-{index}.md")),
            format!("InkFlow {}\n", "x".repeat(500)),
        )
        .unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_inkflow-cli"))
        .args([
            "--format",
            "jsonl",
            "workspace",
            "search",
            &path(temp.path()),
            "InkFlow",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).unwrap();
    assert!(!first_line.is_empty());
    drop(reader);

    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn render_output_confirmation_happens_before_starting_webview() {
    let temp = tempfile::tempdir().unwrap();
    let document = temp.path().join("note.md");
    let output_path = temp.path().join("fragment.html");
    std::fs::write(&document, "# Note\n").unwrap();
    std::fs::write(&output_path, "external").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "render",
            "fragment",
            &path(&document),
            "--output",
            &path(&output_path),
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(5));
    assert_eq!(parse(&output)["error"]["code"], "confirmation_required");
    assert_eq!(std::fs::read_to_string(output_path).unwrap(), "external");
}

#[test]
fn asset_read_rejects_a_directory_as_its_document_scope() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let data = temp.path().join("data");
    let outside = temp.path().join("outside.png");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(&outside, b"outside root").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "--root",
            &path(&root),
            "asset",
            "read",
            "--document",
            &path(&root),
            "outside.png",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(parse(&output)["error"]["code"], "not_a_file");
}

#[test]
fn asset_add_rejects_a_directory_as_its_document_scope() {
    let temp = tempfile::tempdir().unwrap();
    let document_directory = temp.path().join("notes");
    let image = temp.path().join("image.png");
    std::fs::create_dir(&document_directory).unwrap();
    std::fs::write(&image, b"image").unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "asset",
            "add",
            "--document",
            &path(&document_directory),
            "--source",
            &path(&image),
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(parse(&output)["error"]["code"], "not_a_file");
    assert!(!temp.path().join("notes.assets").exists());
}

#[test]
fn render_document_path_must_be_a_file() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "--root",
            &path(&root),
            "render",
            "fragment",
            "-",
            "--document-path",
            &path(&root),
        ],
        Some("# Note\n"),
    );

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(parse(&output)["error"]["code"], "not_a_file");
}

#[test]
fn root_restriction_applies_to_paths_embedded_in_session_updates() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let outside_document = outside.path().join("outside.md");
    std::fs::write(&outside_document, "outside").unwrap();
    let request = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": null,
        "tabs": [{ "path": path(&outside_document), "mode": "live" }],
        "activePath": path(&outside_document)
    })
    .to_string();

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "--root",
            &path(root.path()),
            "session",
            "update",
            "--input",
            "-",
        ],
        Some(&request),
    );

    assert_eq!(output.status.code(), Some(3));
    assert_eq!(parse(&output)["error"]["code"], "path_outside_workspace");
    assert!(!data.join("session.json").exists());
}

#[test]
fn root_restriction_applies_to_request_and_content_input_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();
    let document = root.join("note.md");
    std::fs::write(&document, "inside").unwrap();
    let outside = temp.path().join("outside.json");
    std::fs::write(
        &outside,
        r#"{"schemaVersion":1,"expectedRevision":null,"operations":[]}"#,
    )
    .unwrap();

    let edit = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "--root",
            &path(&root),
            "document",
            "edit",
            &path(&document),
            "--request",
            &path(&outside),
        ],
        None,
    );

    assert_eq!(edit.status.code(), Some(3));
    assert_eq!(parse(&edit)["error"]["code"], "path_outside_workspace");

    let write = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &path(&data),
            "--root",
            &path(&root),
            "document",
            "write",
            &path(&document),
            "--input",
            &path(&outside),
            "--force",
        ],
        None,
    );

    assert_eq!(write.status.code(), Some(3));
    assert_eq!(parse(&write)["error"]["code"], "path_outside_workspace");
    assert_eq!(std::fs::read_to_string(document).unwrap(), "inside");
}

#[test]
fn root_scopes_paths_returned_from_settings_and_session() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();
    let inside = root.join("inside.md");
    let outside = temp.path().join("outside.md");
    std::fs::write(&inside, "inside").unwrap();
    std::fs::write(&outside, "outside").unwrap();
    let data_arg = path(&data);
    let root_arg = path(&root);
    let inside_arg = path(&inside);
    let outside_arg = path(&outside);

    let settings_patch = serde_json::json!({
        "recentFiles": [&inside_arg, &outside_arg]
    })
    .to_string();
    let patch = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "settings",
            "patch",
        ],
        Some(&settings_patch),
    );
    assert_eq!(patch.status.code(), Some(0));

    let settings = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "settings",
            "get",
        ],
        None,
    );
    let settings = parse(&settings);
    assert_eq!(settings["data"]["recentFiles"].as_array().unwrap().len(), 1);
    assert_eq!(settings["data"]["recentFiles"][0], inside_arg);
    assert_eq!(settings["warnings"].as_array().unwrap().len(), 1);

    let session_update = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": &root_arg,
        "tabs": [
            { "path": &inside_arg, "mode": "live" },
            { "path": &outside_arg, "mode": "source" }
        ],
        "activePath": &outside_arg
    })
    .to_string();
    let update = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "session",
            "update",
            "--force",
        ],
        Some(&session_update),
    );
    assert_eq!(update.status.code(), Some(0));

    let session = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "session",
            "get",
        ],
        None,
    );
    let session = parse(&session);
    assert_eq!(
        session["data"]["session"]["tabs"].as_array().unwrap().len(),
        1
    );
    assert_eq!(session["data"]["session"]["activePath"], inside_arg);
    assert_eq!(session["warnings"].as_array().unwrap().len(), 1);

    let scoped_update = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": &root_arg,
        "tabs": [{ "path": &inside_arg, "mode": "preview" }],
        "activePath": &inside_arg
    })
    .to_string();
    let update = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "session",
            "update",
            "--force",
        ],
        Some(&scoped_update),
    );
    assert_eq!(update.status.code(), Some(0));

    let unscoped = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "session",
            "get",
        ],
        None,
    );
    assert_eq!(unscoped.status.code(), Some(0));
    let tabs = parse(&unscoped)["data"]["session"]["tabs"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tabs.len(), 2);
    assert!(tabs.iter().any(|tab| tab["path"] == outside_arg));
    assert!(
        tabs.iter()
            .any(|tab| { tab["path"] == inside_arg && tab["mode"] == "preview" })
    );
}

#[test]
fn scoped_settings_patch_preserves_paths_hidden_outside_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside_root = temp.path().join("outside");
    let data = temp.path().join("data");
    let old_inside = root.join("old.md");
    let new_inside = root.join("new.md");
    let outside = outside_root.join("outside.md");
    let old_workspace = root.join("old-workspace");
    let new_workspace = root.join("new-workspace");
    std::fs::create_dir_all(&old_workspace).unwrap();
    std::fs::create_dir(&new_workspace).unwrap();
    std::fs::create_dir(&outside_root).unwrap();
    std::fs::write(&old_inside, "old").unwrap();
    std::fs::write(&new_inside, "new").unwrap();
    std::fs::write(&outside, "outside").unwrap();
    let data_arg = path(&data);
    let root_arg = path(&root);
    let old_inside_arg = path(&old_inside);
    let new_inside_arg = path(&new_inside);
    let outside_arg = path(&outside);
    let old_workspace_arg = path(&old_workspace);
    let new_workspace_arg = path(&new_workspace);
    let outside_root_arg = path(&outside_root);

    let seed = serde_json::json!({
        "recentFiles": [&old_inside_arg, &outside_arg],
        "recentWorkspaces": [&old_workspace_arg, &outside_root_arg]
    })
    .to_string();
    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "settings",
            "patch",
        ],
        Some(&seed),
    );
    assert_eq!(output.status.code(), Some(0));

    let scoped = serde_json::json!({
        "recentFiles": [&new_inside_arg],
        "recentWorkspaces": [&new_workspace_arg]
    })
    .to_string();
    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "settings",
            "patch",
        ],
        Some(&scoped),
    );
    assert_eq!(output.status.code(), Some(0));

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "settings",
            "get",
        ],
        None,
    );
    assert_eq!(output.status.code(), Some(0));
    let settings = parse(&output)["data"].clone();
    let recent_files = settings["recentFiles"].as_array().unwrap();
    let recent_workspaces = settings["recentWorkspaces"].as_array().unwrap();
    assert!(recent_files.iter().any(|value| value == &outside_arg));
    assert!(recent_files.iter().any(|value| value == &new_inside_arg));
    assert!(!recent_files.iter().any(|value| value == &old_inside_arg));
    assert!(
        recent_workspaces
            .iter()
            .any(|value| value == &outside_root_arg)
    );
    assert!(
        recent_workspaces
            .iter()
            .any(|value| value == &new_workspace_arg)
    );
    assert!(
        !recent_workspaces
            .iter()
            .any(|value| value == &old_workspace_arg)
    );
}

#[test]
fn scoped_settings_patch_reports_entries_dropped_by_hidden_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside_root = temp.path().join("outside");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside_root).unwrap();
    let data_arg = path(&data);
    let root_arg = path(&root);
    let hidden_files = (0..20)
        .map(|index| {
            let hidden = outside_root.join(format!("hidden-{index}.md"));
            std::fs::write(&hidden, "hidden").unwrap();
            path(&hidden)
        })
        .collect::<Vec<_>>();
    let seed = serde_json::json!({ "recentFiles": hidden_files }).to_string();
    let seeded = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "settings",
            "patch",
        ],
        Some(&seed),
    );
    assert_eq!(seeded.status.code(), Some(0));

    let requested = root.join("requested.md");
    std::fs::write(&requested, "requested").unwrap();
    let requested_arg = path(&requested);
    let patch = serde_json::json!({ "recentFiles": [&requested_arg] }).to_string();
    let updated = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "settings",
            "patch",
        ],
        Some(&patch),
    );

    assert_eq!(updated.status.code(), Some(6));
    let envelope = assert_envelope(&updated, "settings.patch", true);
    assert!(
        envelope["data"]["recentFiles"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| {
                warning
                    .as_str()
                    .is_some_and(|warning| warning.contains("1 requested in-root recent file(s)"))
            })
    );

    let unscoped = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "settings",
            "get",
        ],
        None,
    );
    assert_eq!(unscoped.status.code(), Some(0));
    let recent_files = parse(&unscoped)["data"]["recentFiles"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(recent_files.len(), 20);
    assert!(!recent_files.iter().any(|value| value == &requested_arg));
}

#[test]
fn settings_patch_preserves_an_invalid_settings_file() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let settings = data.join("settings.json");
    let invalid = br#"{"theme":"dark"#;
    std::fs::write(&settings, invalid).unwrap();
    let data_arg = path(&data);

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "settings",
            "patch",
        ],
        Some(r#"{"fontSize":20}"#),
    );

    assert_eq!(output.status.code(), Some(3));
    let envelope = assert_envelope(&output, "settings.patch", false);
    assert_eq!(envelope["error"]["code"], "settings_load_error");
    assert_eq!(std::fs::read(settings).unwrap(), invalid);
}

#[test]
fn settings_get_reports_and_preserves_an_invalid_settings_file() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let settings = data.join("settings.json");
    let invalid = br#"{"theme":"dark"#;
    std::fs::write(&settings, invalid).unwrap();
    let data_arg = path(&data);

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "settings",
            "get",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(3));
    let envelope = assert_envelope(&output, "settings.get", false);
    assert_eq!(envelope["error"]["code"], "settings_load_error");
    assert_eq!(std::fs::read(settings).unwrap(), invalid);
}

#[test]
fn deleting_a_missing_recovery_snapshot_returns_an_error() {
    let temp = tempfile::tempdir().unwrap();
    let data_arg = path(&temp.path().join("data"));

    let output = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "recovery",
            "delete",
            "missing-snapshot",
            "--yes",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(3));
    let envelope = assert_envelope(&output, "recovery.delete", false);
    assert_eq!(envelope["error"]["code"], "recovery_not_found");
}

#[test]
fn scoped_session_clear_preserves_hidden_workspace_state() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside_root = temp.path().join("outside");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside_root).unwrap();
    let inside = root.join("inside.md");
    let outside = outside_root.join("outside.md");
    std::fs::write(&inside, "inside").unwrap();
    std::fs::write(&outside, "outside").unwrap();
    let data_arg = path(&data);
    let root_arg = path(&root);
    let inside_arg = path(&inside);
    let outside_arg = path(&outside);
    let outside_root_arg = path(&outside_root);
    let session = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": &outside_root_arg,
        "tabs": [
            { "path": &inside_arg, "mode": "preview" },
            { "path": &outside_arg, "mode": "source" }
        ],
        "activePath": &outside_arg
    })
    .to_string();
    let seed = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "session",
            "update",
            "--force",
        ],
        Some(&session),
    );
    assert_eq!(seed.status.code(), Some(0));

    let clear = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "session",
            "clear",
            "--yes",
        ],
        None,
    );
    assert_eq!(clear.status.code(), Some(0));
    let clear = assert_envelope(&clear, "session.clear", true);
    assert_eq!(clear["data"]["workspaceRoot"], Value::Null);
    assert_eq!(clear["data"]["activePath"], Value::Null);
    assert!(clear["data"]["tabs"].as_array().unwrap().is_empty());
    assert_eq!(clear["warnings"].as_array().unwrap().len(), 1);

    let unscoped = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "session",
            "get",
        ],
        None,
    );
    assert_eq!(unscoped.status.code(), Some(0));
    let unscoped = parse(&unscoped);
    assert_eq!(
        unscoped["data"]["session"]["workspaceRoot"],
        outside_root_arg
    );
    assert_eq!(unscoped["data"]["session"]["activePath"], outside_arg);
    let tabs = unscoped["data"]["session"]["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0]["path"], outside_arg);
}

#[test]
fn scoped_session_update_preserves_hidden_workspace_and_active_path() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside_root = temp.path().join("outside");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside_root).unwrap();
    let inside = root.join("inside.md");
    let outside = outside_root.join("outside.md");
    std::fs::write(&inside, "inside").unwrap();
    std::fs::write(&outside, "outside").unwrap();
    let data_arg = path(&data);
    let root_arg = path(&root);
    let outside_root_arg = path(&outside_root);
    let inside_arg = path(&inside);
    let outside_arg = path(&outside);

    let initial = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": &outside_root_arg,
        "tabs": [
            { "path": &outside_arg, "mode": "source" },
            { "path": &inside_arg, "mode": "live" }
        ],
        "activePath": &outside_arg
    })
    .to_string();
    let seeded = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "session",
            "update",
            "--force",
        ],
        Some(&initial),
    );
    assert_eq!(seeded.status.code(), Some(0));

    let scoped = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": &root_arg,
        "tabs": [{ "path": &inside_arg, "mode": "preview" }],
        "activePath": &inside_arg
    })
    .to_string();
    let updated = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "session",
            "update",
            "--force",
        ],
        Some(&scoped),
    );
    assert_eq!(updated.status.code(), Some(0));

    let unscoped = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "session",
            "get",
        ],
        None,
    );
    assert_eq!(unscoped.status.code(), Some(0));
    let session = parse(&unscoped)["data"]["session"].clone();
    assert_eq!(session["workspaceRoot"], outside_root_arg);
    assert_eq!(session["activePath"], outside_arg);
    let tabs = session["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);
    assert!(
        tabs.iter()
            .any(|tab| tab["path"] == inside_arg && tab["mode"] == "preview")
    );
    assert!(tabs.iter().any(|tab| tab["path"] == outside_arg));
}

#[test]
fn scoped_session_update_reports_tabs_dropped_by_hidden_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let outside_root = temp.path().join("outside");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside_root).unwrap();
    let data_arg = path(&data);
    let root_arg = path(&root);
    let hidden_tabs = (0..49)
        .map(|index| {
            serde_json::json!({
                "path": path(&outside_root.join(format!("hidden-{index}.md"))),
                "mode": "live"
            })
        })
        .collect::<Vec<_>>();
    let initial = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": null,
        "tabs": hidden_tabs,
        "activePath": null
    })
    .to_string();
    let seeded = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "session",
            "update",
            "--force",
        ],
        Some(&initial),
    );
    assert_eq!(seeded.status.code(), Some(0));

    let first = path(&root.join("first.md"));
    let second = path(&root.join("second.md"));
    let scoped = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": &root_arg,
        "tabs": [
            { "path": &first, "mode": "live" },
            { "path": &second, "mode": "preview" }
        ],
        "activePath": &second
    })
    .to_string();
    let updated = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "session",
            "update",
            "--force",
        ],
        Some(&scoped),
    );

    assert_eq!(updated.status.code(), Some(6));
    let envelope = assert_envelope(&updated, "session.update", true);
    assert_eq!(
        envelope["data"]["session"]["tabs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(envelope["data"]["session"]["tabs"][0]["path"], first);
    assert!(
        envelope["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| {
                warning
                    .as_str()
                    .is_some_and(|warning| warning.contains("1 requested in-root session tab(s)"))
            })
    );

    let unscoped = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "session",
            "get",
        ],
        None,
    );
    assert_eq!(unscoped.status.code(), Some(0));
    assert_eq!(
        parse(&unscoped)["data"]["session"]["tabs"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
}

#[test]
fn every_subcommand_has_a_versioned_process_contract() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let data = temp.path().join("data");
    std::fs::create_dir(&root).unwrap();
    let document = root.join("note.md");
    std::fs::write(
        &document,
        "# Title\n\nalpha beta\n\n- [ ] task\n\n| A |\n| --- |\n| x |\n",
    )
    .unwrap();
    let image = root.join("image.png");
    std::fs::write(&image, b"png fixture").unwrap();
    let document_arg = path(&document);
    let root_arg = path(&root);
    let data_arg = path(&data);
    let base = ["--format", "json", "--data-dir", data_arg.as_str()];

    let invoke = |arguments: &[&str], stdin: Option<&str>| {
        let args = base
            .iter()
            .copied()
            .chain(arguments.iter().copied())
            .collect::<Vec<_>>();
        run(&args, stdin)
    };

    for (command, arguments) in [
        ("capabilities", vec!["capabilities"]),
        ("schema", vec!["schema"]),
        ("document.read", vec!["document", "read", &document_arg]),
        (
            "document.analyze",
            vec!["document", "analyze", &document_arg],
        ),
        (
            "document.search",
            vec!["document", "search", &document_arg, "alpha"],
        ),
        (
            "document.replace",
            vec![
                "document",
                "replace",
                &document_arg,
                "alpha",
                "ALPHA",
                "--dry-run",
            ],
        ),
    ] {
        let output = invoke(&arguments, None);
        assert_eq!(output.status.code(), Some(0), "{command}");
        assert_envelope(&output, command, true);
    }

    let edit_request = r#"{"schemaVersion":1,"expectedRevision":null,"operations":[]}"#;
    let edit = invoke(
        &[
            "document",
            "edit",
            &document_arg,
            "--request",
            "-",
            "--dry-run",
        ],
        Some(edit_request),
    );
    assert_eq!(edit.status.code(), Some(0));
    assert_envelope(&edit, "document.edit", true);

    let written = root.join("written.md");
    let written_arg = path(&written);
    let write = invoke(
        &[
            "document",
            "write",
            &written_arg,
            "--create",
            "--input",
            "-",
        ],
        Some("written"),
    );
    assert_eq!(write.status.code(), Some(0));
    assert_envelope(&write, "document.write", true);

    let copy = root.join("copy.md");
    let copy_arg = path(&copy);
    let save_as = invoke(
        &["document", "save-as", &document_arg, &copy_arg, "--dry-run"],
        None,
    );
    assert_eq!(save_as.status.code(), Some(0));
    assert_envelope(&save_as, "document.saveAs", true);

    for (command, arguments) in [
        ("workspace.tree", vec!["workspace", "tree", &root_arg]),
        (
            "workspace.search",
            vec!["workspace", "search", &root_arg, "alpha"],
        ),
        (
            "workspace.create",
            vec!["workspace", "create", &root_arg, ".", "created.md"],
        ),
        (
            "workspace.rename",
            vec!["workspace", "rename", &root_arg, "created.md", "renamed.md"],
        ),
        (
            "workspace.trash",
            vec!["workspace", "trash", &root_arg, "renamed.md", "--dry-run"],
        ),
    ] {
        let output = invoke(&arguments, None);
        assert_eq!(output.status.code(), Some(0), "{command}");
        assert_envelope(&output, command, true);
    }

    let image_arg = path(&image);
    let asset_add = invoke(
        &[
            "asset",
            "add",
            "--document",
            &document_arg,
            "--source",
            &image_arg,
        ],
        None,
    );
    assert_eq!(asset_add.status.code(), Some(0));
    let asset = assert_envelope(&asset_add, "asset.add", true);
    let markdown_path = asset["data"]["markdownPath"].as_str().unwrap();
    let asset_read = invoke(
        &["asset", "read", "--document", &document_arg, markdown_path],
        None,
    );
    assert_eq!(asset_read.status.code(), Some(0));
    assert_envelope(&asset_read, "asset.read", true);

    let recovery_list = invoke(&["recovery", "list"], None);
    assert_eq!(recovery_list.status.code(), Some(0));
    assert_envelope(&recovery_list, "recovery.list", true);
    let checkpoint = invoke(&["recovery", "checkpoint", &document_arg], None);
    assert_eq!(checkpoint.status.code(), Some(0));
    let checkpoint = assert_envelope(&checkpoint, "recovery.checkpoint", true);
    let recovery_id = checkpoint["data"]["entry"]["id"].as_str().unwrap();
    let restore = invoke(&["recovery", "restore", recovery_id], None);
    assert_eq!(restore.status.code(), Some(0));
    assert_envelope(&restore, "recovery.restore", true);
    let delete = invoke(&["recovery", "delete", recovery_id, "--yes"], None);
    assert_eq!(delete.status.code(), Some(0));
    assert_envelope(&delete, "recovery.delete", true);

    let settings_get = invoke(&["settings", "get"], None);
    assert_eq!(settings_get.status.code(), Some(0));
    assert_envelope(&settings_get, "settings.get", true);
    let settings_patch = invoke(&["settings", "patch"], Some(r#"{"theme":"dark"}"#));
    assert_eq!(settings_patch.status.code(), Some(0));
    assert_envelope(&settings_patch, "settings.patch", true);
    let settings_reset = invoke(&["settings", "reset"], None);
    assert_eq!(settings_reset.status.code(), Some(0));
    assert_envelope(&settings_reset, "settings.reset", true);

    let session_get = invoke(&["session", "get"], None);
    assert_eq!(session_get.status.code(), Some(0));
    assert_envelope(&session_get, "session.get", true);
    let session_request = serde_json::json!({
        "schemaVersion": 1,
        "workspaceRoot": &root_arg,
        "tabs": [{ "path": &document_arg, "mode": "live" }],
        "activePath": &document_arg
    })
    .to_string();
    let session_update = invoke(&["session", "update", "--force"], Some(&session_request));
    assert_eq!(session_update.status.code(), Some(0));
    assert_envelope(&session_update, "session.update", true);
    let session_clear = invoke(&["session", "clear", "--yes"], None);
    assert_eq!(session_clear.status.code(), Some(0));
    assert_envelope(&session_clear, "session.clear", true);

    let render_output = root.join("existing-fragment.html");
    let html_output = root.join("existing-export.html");
    let pdf_output = root.join("existing-export.pdf");
    std::fs::write(&render_output, "existing").unwrap();
    std::fs::write(&html_output, "existing").unwrap();
    std::fs::write(&pdf_output, "existing").unwrap();
    let render_output_arg = path(&render_output);
    let html_output_arg = path(&html_output);
    let pdf_output_arg = path(&pdf_output);
    for (command, arguments) in [
        (
            "render.fragment",
            vec![
                "render",
                "fragment",
                &document_arg,
                "--output",
                &render_output_arg,
            ],
        ),
        (
            "export.html",
            vec![
                "export",
                "html",
                &document_arg,
                "--output",
                &html_output_arg,
            ],
        ),
        (
            "export.pdf",
            vec!["export", "pdf", &document_arg, "--output", &pdf_output_arg],
        ),
    ] {
        let output = invoke(&arguments, None);
        assert_eq!(output.status.code(), Some(5), "{command}");
        assert_envelope(&output, command, false);
    }

    let outside = temp.path().join("outside.md");
    std::fs::write(&outside, "outside").unwrap();
    let app_open = run(
        &[
            "--format",
            "json",
            "--data-dir",
            &data_arg,
            "--root",
            &root_arg,
            "app",
            "open",
            &path(&outside),
        ],
        None,
    );
    assert!(!app_open.status.success());
    assert_envelope(&app_open, "app.open", false);
}
