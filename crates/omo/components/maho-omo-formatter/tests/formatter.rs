use maho_ext_api::{ToolContent, ToolResultEvent};
use maho_omo_formatter::*;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn test_daemon_format_success_emits_notice_block() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("src/a.ts");
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(&file_path, "const a=1;\n").unwrap();

    let step = create_formatter_step(FormatterStepOptions {
        config: Some(FormatOnMutationConfig {
            mode: Some(FormatMode::BestEffort),
            ..Default::default()
        }),
        markers: Some(Box::new(|_| vec!["biome.json".to_string()])),
        daemon_format: Some(Box::new(|_| Box::pin(async {
            Ok(DaemonFormatResult {
                content: None,
                details: Some(json!({
                    "status": "formatted",
                    "linesAdded": 2,
                    "linesRemoved": 1
                })),
                is_error: false,
            })
        }))),
        ..Default::default()
    });

    let event = ToolResultEvent {
        tool_call_id: "1".into(),
        tool_name: "write".into(),
        input: json!({ "filePath": "src/a.ts" }),
        content: vec![],
        details: None,
        is_error: false,
        usage: None,
    };

    let outcome = step.format_tool_result(&event, dir.path(), Some("test-session")).await;
    assert!(outcome.error.is_none());
    assert!(outcome.content.is_some());

    let blocks = outcome.content.unwrap();
    assert_eq!(blocks.len(), 1);
    if let ToolContent::Text { text, .. } = &blocks[0] {
        assert!(text.contains("(OmO) auto-formatted src/a.ts with biome (+2/-1 lines)"));
        assert!(text.contains("File content changed; re-read before exact-text edits."));
    } else {
        panic!("Expected text block");
    }
}

#[tokio::test]
async fn test_off_mode_produces_no_actions() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("a.ts");
    std::fs::write(&file_path, "const a=1;\n").unwrap();

    let step = create_formatter_step(FormatterStepOptions {
        config: Some(FormatOnMutationConfig {
            mode: Some(FormatMode::Off),
            ..Default::default()
        }),
        markers: Some(Box::new(|_| vec!["biome.json".to_string()])),
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
    assert!(outcome.content.is_none());
    assert!(outcome.error.is_none());
}
