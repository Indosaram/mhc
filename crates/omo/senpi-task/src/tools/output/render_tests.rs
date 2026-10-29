//! `tools/output/render.test.ts`

use pretty_assertions::assert_eq;

use crate::tools::output::render::{RenderOptions, TRANSCRIPT_MAX_CHARS, render_transcript};
use crate::tools::output::types::{TranscriptEntry, TranscriptMode};

fn assistant(text: &str) -> TranscriptEntry {
    TranscriptEntry::Assistant { text: text.to_string() }
}

fn tool(name: &str, is_error: bool) -> TranscriptEntry {
    TranscriptEntry::Tool {
        tool: name.to_string(),
        is_error,
    }
}

fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[test]
fn given_entries_when_full_mode_then_every_entry_is_rendered_and_not_truncated() {
    // given
    let entries = vec![assistant("first answer"), tool("bash", false), assistant("second answer")];

    // when
    let rendered = render_transcript(
        &entries,
        &RenderOptions {
            mode: TranscriptMode::Full,
            tail_lines: 60,
        },
    );

    // then
    assert!(rendered.text.contains("first answer"));
    assert!(rendered.text.contains("bash"));
    assert!(rendered.text.contains("second answer"));
    assert_eq!(rendered.truncated, false);
}

#[test]
fn given_a_transcript_with_more_lines_than_tail_lines_when_tail_mode_then_only_the_last_tail_lines_lines_remain() {
    // given
    let entries: Vec<TranscriptEntry> = (0..10).map(|index| assistant(&format!("line {index}"))).collect();

    // when
    let rendered = render_transcript(
        &entries,
        &RenderOptions {
            mode: TranscriptMode::Tail,
            tail_lines: 3,
        },
    );

    // then
    let lines: Vec<&str> = rendered.text.trim().split('\n').collect();
    assert_eq!(lines.len(), 3);
    assert!(rendered.text.contains("line 9"));
    assert!(!rendered.text.contains("line 0"));
}

#[test]
fn given_a_transcript_longer_than_the_cap_when_rendered_then_it_is_elided_with_a_head_tail_marker_under_the_cap() {
    // given
    let big = "x".repeat(TRANSCRIPT_MAX_CHARS * 2);
    let entries = vec![assistant(&big)];

    // when
    let rendered = render_transcript(
        &entries,
        &RenderOptions {
            mode: TranscriptMode::Full,
            tail_lines: 60,
        },
    );

    // then
    assert!(js_len(&rendered.text) <= TRANSCRIPT_MAX_CHARS);
    assert!(rendered.text.contains("elided"));
    assert_eq!(rendered.truncated, true);
}

#[test]
fn given_no_entries_when_rendered_then_an_empty_transcript_notice_is_returned_and_not_truncated() {
    // given / when
    let rendered = render_transcript(
        &[],
        &RenderOptions {
            mode: TranscriptMode::Full,
            tail_lines: 60,
        },
    );

    // then
    assert_eq!(rendered.truncated, false);
    assert!(js_len(&rendered.text) > 0);
}

#[test]
fn given_an_error_transcript_entry_when_rendered_then_it_is_shown_as_an_error_line() {
    // given
    let entries = vec![TranscriptEntry::Error {
        message: "upstream gateway timeout".to_string(),
    }];

    // when
    let rendered = render_transcript(
        &entries,
        &RenderOptions {
            mode: TranscriptMode::Full,
            tail_lines: 0,
        },
    );

    // then
    assert_eq!(rendered.text, "error: upstream gateway timeout");
}
