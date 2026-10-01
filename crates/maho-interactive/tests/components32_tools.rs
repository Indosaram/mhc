use maho_interactive::components::diff::render_diff;
use maho_interactive::components::render_signature::create_bounded_render_signature;
use maho_interactive::components::visual_truncate::truncate_to_visual_lines;
use maho_interactive::grok_mermaid::source_box;
use maho_interactive::jsdiff::diff_words;
use maho_interactive::theme::{ColorMode, Theme};
use serde_json::Value;

fn theme() -> Theme {
    Theme::builtin("dark", ColorMode::Color256).expect("builtin theme")
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("golden/components32-tools.json")).expect("pinned fixture")
}

#[test]
fn render_signatures_match_pinned_senpi_for_every_bounded_shape() {
    for case in fixture()["signatures"].as_array().expect("signatures") {
        assert_eq!(
            create_bounded_render_signature(&case["value"]),
            case["signature"].as_str().expect("signature"),
            "signature for {}",
            case["value"]
        );
    }
}

#[test]
fn source_boxes_match_pinned_senpi_at_every_wrapping_width() {
    for case in fixture()["sourceBoxes"].as_array().expect("sourceBoxes") {
        let src = case["src"].as_str().expect("src");
        let width = case["width"].as_u64().map(|width| width as usize);
        let art = source_box(src, width);
        let expected: Vec<String> =
            case["art"]["plain"].as_array().expect("plain").iter().map(|line| line.as_str().expect("line").to_owned()).collect();
        assert_eq!(art.plain, expected, "source box for {src:?} at {width:?}");
        assert_eq!(art.width, case["art"]["width"].as_u64().expect("width") as usize);
        assert_eq!(art.width, art.plain.iter().map(|line| line.chars().count()).max().unwrap_or(0));
    }
}

#[test]
fn word_diff_keeps_common_text_and_marks_only_changes() {
    let changes = diff_words("the quick brown fox", "the quick red fox");
    let kept: String = changes.iter().filter(|change| !change.added && !change.removed).map(|change| change.value.as_str()).collect();
    let added: String = changes.iter().filter(|change| change.added).map(|change| change.value.as_str()).collect();
    let removed: String = changes.iter().filter(|change| change.removed).map(|change| change.value.as_str()).collect();
    assert_eq!(kept, "the quick  fox");
    assert_eq!(added, "red");
    assert_eq!(removed, "brown");
}

#[test]
fn word_diff_reports_a_pure_insertion_as_one_added_run() {
    let changes = diff_words("foo baz", "foo bar baz");
    assert!(changes.iter().any(|change| change.added && change.value.contains("bar")));
    assert!(!changes.iter().any(|change| change.removed));
}

#[test]
fn diff_render_colours_removed_and_added_lines_and_highlights_intra_line_changes() {
    let rendered = render_diff("-1 old value\n+1 new value\n 2 kept", &theme());
    let lines: Vec<&str> = rendered.split('\n').collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("old"), "{rendered:?}");
    assert!(lines[1].contains("new"), "{rendered:?}");
    assert!(lines[2].contains("kept"), "{rendered:?}");
    assert_ne!(lines[0], lines[1]);
}

#[test]
fn diff_render_treats_unparseable_lines_as_context() {
    let rendered = render_diff("not a diff line", &theme());
    assert!(rendered.contains("not a diff line"));
}

#[test]
fn visual_truncation_keeps_the_tail_and_counts_the_hidden_prefix() {
    let text = (1..=30).map(|index| format!("line {index}")).collect::<Vec<_>>().join("\n");
    let result = truncate_to_visual_lines(&text, 5, 40, 0);
    assert_eq!(result.visual_lines.len(), 5);
    assert_eq!(result.skipped_count, 25);
    assert_eq!(result.visual_lines.last().map(|line| line.trim_end()), Some("line 30"));
}

#[test]
fn visual_truncation_passes_short_text_through_unchanged() {
    let result = truncate_to_visual_lines("one\ntwo", 5, 40, 0);
    assert_eq!(result.skipped_count, 0);
    assert_eq!(result.visual_lines.len(), 2);
}
