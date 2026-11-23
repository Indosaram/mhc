use maho_tools::{definition::*, read::*, truncate::*};
use serde_json::json;
use std::sync::Arc;

struct FullTextFolder;

impl ReadFolder for FullTextFolder {
    fn summarize<'a>(&'a self, input: ReadSummaryInput<'a>) -> ToolFuture<'a, Option<String>> {
        Box::pin(async move {
            assert_eq!(input.offset, Some(2));
            assert_eq!(input.limit, Some(1));
            assert!(input.truncated);
            Ok(Some(input.text.to_owned()))
        })
    }
}

#[tokio::test]
async fn custom_folder_can_recover_full_lossy_text_from_truncated_window() {
    // Given invalid UTF-8 outside an oversized requested line and a trailing empty line.
    let dir = tempfile::tempdir().expect("fixture directory");
    let bytes = [
        b"\xff\r\n".as_slice(),
        &vec![b'a'; DEFAULT_MAX_BYTES + 1],
        b"\nend\n",
    ]
    .concat();
    std::fs::write(dir.path().join("fixture"), &bytes).expect("fixture file");
    let tool = create_read_tool_definition(
        dir.path().into(),
        ReadToolOptions {
            folder: Some(Arc::new(FullTextFolder)),
            ..Default::default()
        },
    );
    // When the custom folder chooses to return the original file as its summary.
    let result = (tool.execute)(ToolCall {
        id: "folder",
        params: json!({"path":"fixture","offset":2,"limit":1}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("read tool result");
    // Then the summary supersedes truncation, with no truncation details or notice.
    let [
        ToolContent::Text {
            text,
            audience: None,
        },
    ] = result.content.as_slice()
    else {
        panic!("expected a single visible summary");
    };
    assert_eq!(text.as_str(), String::from_utf8_lossy(&bytes));
    eprintln!(
        "RP2_OWNER {}",
        serde_json::json!({"tag": "folder", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": {"length":text.len(),"prefix":text.get(..5),"suffix":text.get(text.len()-5..),"aCount":text.bytes().filter(|b| *b == b'a').count(),"detailsAbsent":result.details.is_none()}})
    );
    assert!(result.details.is_none());
}

#[test]
fn truncation_priority_when_partial_tail_reaches_line_cap() {
    // Given one permitted line and bytes that cut inside the final UTF-8 character.
    let options = TruncationOptions {
        max_lines: 1,
        max_bytes: 2,
    };
    // When truncating from the tail, the final LF is a terminator rather than its own line.
    let result = truncate_tail("one\r\n한\n", options);
    // Then the partial line counts even when no whole character fit, and lines take priority.
    assert_eq!(result.truncated_by.as_deref(), Some("lines"));
    assert!(result.last_line_partial);
    assert_eq!(
        (result.total_lines, result.output_lines, result.output_bytes),
        (2, 1, 0)
    );
    eprintln!(
        "RP2_OWNER {}",
        serde_json::json!({"tag": "cap", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": result})
    );
    assert_eq!(result.content, "");
}
