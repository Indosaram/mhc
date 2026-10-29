use maho_ext_api::ToolResultEvent;
use maho_omo_formatter::*;
use serde_json::json;
use std::path::Path;
use tempfile::tempdir;

#[tokio::test]
async fn test_daemon_ok_with_missing_or_nonobject_details_returns_unchanged() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.ts");
    std::fs::write(&file_path, "const x = 1;\n").unwrap();

    let formatter = Formatter {
        tool: FormatterTool::Biome,
        command: "biome".to_string(),
        args: vec!["format".to_string(), "--write".to_string()],
    };

    // Daemon returns Ok but details is None (or null)
    let daemon_format: DaemonFormatFn = Box::new(|_| Box::pin(async {
        Ok(DaemonFormatResult {
            content: None,
            details: None,
            is_error: false,
        })
    }));

    let result = format_one(
        &file_path,
        &formatter,
        dir.path(),
        1_000,
        Some(&daemon_format),
        None,
    ).await;

    // Upstream semantics: details is undefined -> returns unchanged, does NOT fall through to CLI
    assert_eq!(result.status, FormatStatus::Unchanged);
    assert_eq!(result.added, 0);
    assert_eq!(result.removed, 0);
}

#[tokio::test]
async fn test_split_newline_count_handles_trailing_newlines() {
    let before = "a\nb\nc";
    let after = "a\nb\nc\n";
    let before_count = before.split('\n').count();
    let after_count = after.split('\n').count();
    assert_eq!(before_count, 3);
    assert_eq!(after_count, 4);
    assert_eq!(after_count - before_count, 1);
}

#[test]
fn test_normalize_resolved_path_lexical() {
    let cwd = Path::new("/workspace/project");
    assert_eq!(
        normalize_resolved_path(cwd, "src/../src/a.ts"),
        Path::new("/workspace/project/src/a.ts")
    );
    assert_eq!(
        normalize_resolved_path(cwd, "./sub/./b.rs"),
        Path::new("/workspace/project/sub/b.rs")
    );
}

#[tokio::test]
async fn test_no_markers_provided_defaults_to_empty_and_does_not_format() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("a.ts");
    std::fs::write(&file_path, "const a=1;\n").unwrap();

    // Default step with NO markers callback
    let step = create_formatter_step(FormatterStepOptions {
        config: Some(FormatOnMutationConfig {
            mode: Some(FormatMode::BestEffort),
            ..Default::default()
        }),
        markers: None, // No markers provided!
        ..Default::default()
    });

    let event = ToolResultEvent {
        tool_call_id: "1".into(),
        tool_name: "write".into(),
        input: json!({ "filePath": "a.ts" }),
        content: vec![],
        details: None,
        is_error: false,
        usage: None,
    };

    let outcome = step.format_tool_result(&event, dir.path(), None).await;
    // Without markers provided, marker_names is empty and no formatter is detected
    assert!(outcome.content.is_none());
    assert!(outcome.error.is_none());
}
