use maho_ext_api::ToolResultEvent;
use maho_omo_formatter::*;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

#[tokio::test]
async fn test_cli_fallback_degrades_when_binary_cannot_be_spawned_in_best_effort() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("best-effort.ts");
    std::fs::write(&file_path, "const a=1\n").unwrap();

    let warnings = Arc::new(Mutex::new(Vec::<String>::new()));
    let warnings_clone = Arc::clone(&warnings);

    let step = create_formatter_step(FormatterStepOptions {
        config: Some(FormatOnMutationConfig {
            mode: Some(FormatMode::BestEffort),
            languages: Default::default(),
            max_file_bytes: None,
            timeout_ms: Some(1_000),
        }),
        markers: Some(Box::new(|_| vec![".prettierrc".to_string()])),
        daemon_format: Some(Box::new(|_| Box::pin(async {
            Ok(DaemonFormatResult {
                content: None,
                details: Some(json!({"status": "unavailable"})),
                is_error: false,
            })
        }))),
        resolve_binary: Some(Box::new(|_, cwd| {
            Some(cwd.join("node_modules/.bin/prettier").to_string_lossy().into_owned())
        })),
        logger: Some(Arc::new(move |msg: &str| {
            warnings_clone.lock().unwrap().push(msg.to_string());
        })),
        ..Default::default()
    });

    let event = ToolResultEvent {
        tool_call_id: "1".into(),
        tool_name: "write".into(),
        input: json!({ "filePath": file_path.to_string_lossy().into_owned() }),
        content: vec![],
        details: None,
        is_error: false,
        usage: None,
    };

    let outcome = step.format_tool_result(&event, dir.path(), Some("s1")).await;
    assert!(outcome.error.is_none());
    assert!(outcome.content.is_none());

    let recorded = warnings.lock().unwrap().clone();
    assert_eq!(recorded, vec!["Formatter prettier unavailable; install it with bun add -d"]);
}

#[tokio::test]
async fn test_cli_fallback_reports_error_in_required_mode() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("required.ts");
    std::fs::write(&file_path, "const a=1\n").unwrap();

    let step = create_formatter_step(FormatterStepOptions {
        config: Some(FormatOnMutationConfig {
            mode: Some(FormatMode::Required),
            languages: Default::default(),
            max_file_bytes: None,
            timeout_ms: Some(1_000),
        }),
        markers: Some(Box::new(|_| vec![".prettierrc".to_string()])),
        daemon_format: Some(Box::new(|_| Box::pin(async {
            Ok(DaemonFormatResult {
                content: None,
                details: Some(json!({"status": "unavailable"})),
                is_error: false,
            })
        }))),
        resolve_binary: Some(Box::new(|_, cwd| {
            Some(cwd.join("node_modules/.bin/prettier").to_string_lossy().into_owned())
        })),
        ..Default::default()
    });

    let event = ToolResultEvent {
        tool_call_id: "1".into(),
        tool_name: "write".into(),
        input: json!({ "filePath": file_path.to_string_lossy().into_owned() }),
        content: vec![],
        details: None,
        is_error: false,
        usage: None,
    };

    let outcome = step.format_tool_result(&event, dir.path(), Some("s1")).await;
    assert!(outcome.error.is_some());
    let err_str = outcome.error.unwrap();
    assert!(err_str.contains("Formatter prettier is unavailable"));
}
