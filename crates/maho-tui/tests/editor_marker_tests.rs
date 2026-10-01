//! Port of senpi `packages/tui/test/image-markers.test.ts` and the paste-marker cases of
//! `editor-paste-expansion.test.ts` / `editor-paste-removal.test.ts`.

mod common;

use std::collections::BTreeSet;

use common::{create_editor, large_paste, paste};
use maho_tui::image_markers::{
    image_marker_id, is_image_marker, ImageMarkerRegistry, IMAGE_MARKER_REGEX, IMAGE_MARKER_SINGLE,
};
use maho_tui::paste_markers::{
    grapheme_segment_refs, is_paste_marker, segment_with_markers, segment_with_paste_markers,
};

fn segments(text: &str, markers: &[&str]) -> Vec<String> {
    let valid: BTreeSet<String> = markers.iter().map(|marker| (*marker).to_string()).collect();
    let base = grapheme_segment_refs(text);
    segment_with_markers(text, &base, &valid)
        .into_iter()
        .map(|segment| segment.segment)
        .collect()
}

fn single_chars(text: &str) -> Vec<String> {
    text.chars().map(|ch| ch.to_string()).collect()
}

#[test]
fn recognizes_canonical_markers_and_rejects_malformed_ones() {
    assert!(is_image_marker("[Image #1]"));
    assert!(is_image_marker("[Image #12]"));
    assert_eq!(image_marker_id("[Image #12]"), Some(12));
    assert_eq!(image_marker_id("[Image #1]"), Some(1));

    for invalid in [
        "[Image #0]",
        "[Image #]",
        "[image #1]",
        "[paste #1 3 chars]",
        "[Image #1",
        "Image #1",
    ] {
        assert!(!is_image_marker(invalid), "{invalid}");
        assert_eq!(image_marker_id(invalid), None, "{invalid}");
    }
}

#[test]
fn keeps_the_single_and_global_regexes_in_sync_on_the_canonical_form() {
    assert!(IMAGE_MARKER_SINGLE.is_match("[Image #3]"));
    assert!(!IMAGE_MARKER_SINGLE.is_match("a [Image #3]"));
    let matched: Vec<&str> = IMAGE_MARKER_REGEX
        .find_iter("b [Image #2] a [Image #1]")
        .map(|m| m.as_str())
        .collect();
    assert_eq!(matched, vec!["[Image #2]", "[Image #1]"]);
}

#[test]
fn does_not_classify_paste_markers_as_image_markers() {
    assert!(!is_image_marker("[paste #1 3 chars]"));
    assert!(!is_paste_marker("[Image #1]"));
}

#[test]
fn issues_canonical_markers_in_insertion_order() {
    let mut registry = ImageMarkerRegistry::new();
    assert_eq!(registry.add(), "[Image #1]");
    assert_eq!(registry.add(), "[Image #2]");
    assert_eq!(registry.snapshot().ids, vec![1, 2]);
    assert_eq!(registry.snapshot().image_counter, 2);
}

#[test]
fn excludes_a_marker_appearing_twice_from_the_authorized_set() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    let authorized = registry.authorized_markers("[Image #1] [Image #2] [Image #1]");
    assert!(authorized.contains("[Image #2]"));
    assert!(!authorized.contains("[Image #1]"));
}

#[test]
fn reports_ids_in_text_reading_order_not_insertion_order() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    assert_eq!(registry.ids("b [Image #2] a [Image #1]"), vec![2, 1]);
    assert_eq!(registry.ids("[Image #1] [Image #2]"), vec![1, 2]);
}

#[test]
fn ignores_unregistered_and_duplicated_markers_when_reading_ids() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    assert_eq!(registry.ids("[Image #1] [Image #7]"), vec![1]);
    assert!(registry.ids("[Image #1] [Image #1]").is_empty());
}

#[test]
fn removes_a_marker_and_renumbers_higher_ids_downward() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    let removal = registry.remove(1, "x [Image #1] y [Image #2]");

    assert!(removal.removed);
    assert_eq!(removal.text, "x  y [Image #1]");
    assert_eq!(registry.ids(&removal.text), vec![1]);
    assert_eq!(registry.snapshot().ids, vec![1]);
    assert_eq!(registry.snapshot().image_counter, 1);
}

#[test]
fn reports_no_removal_for_an_unknown_id() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    let removal = registry.remove(9, "x [Image #1]");
    assert_eq!(removal.text, "x [Image #1]");
    assert!(!removal.removed);
}

#[test]
fn renumbers_so_the_next_add_continues_after_the_survivors() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    let removal = registry.remove(1, "[Image #1] [Image #2]");
    assert_eq!(removal.text, " [Image #1]");
    assert_eq!(registry.add(), "[Image #2]");
}

#[test]
fn canonicalizes_markers_to_reading_order_and_reports_the_original_order() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    let result = registry.canonicalize("b [Image #2] a [Image #1]");

    assert_eq!(result.text, "b [Image #1] a [Image #2]");
    assert_eq!(result.order, vec![2, 1]);
    assert_eq!(registry.ids(&result.text), vec![1, 2]);
}

#[test]
fn leaves_already_canonical_text_untouched() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    let result = registry.canonicalize("[Image #1] [Image #2]");
    assert_eq!(result.text, "[Image #1] [Image #2]");
    assert_eq!(result.order, vec![1, 2]);
}

#[test]
fn prunes_markers_absent_from_the_text() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    registry.prune("only [Image #2] survives", None);

    assert_eq!(registry.snapshot().ids, vec![2]);
    assert_eq!(registry.snapshot().image_counter, 2);
    assert_eq!(registry.ids("only [Image #2] survives"), vec![2]);
}

#[test]
fn prunes_markers_that_were_not_authorized_in_the_previous_text() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.prune("[Image #1]", Some("[Image #1] [Image #1]"));
    assert!(registry.snapshot().ids.is_empty());
    assert_eq!(registry.snapshot().image_counter, 0);
}

#[test]
fn round_trips_through_snapshot_and_restore() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    let state = registry.snapshot();

    let mut restored = ImageMarkerRegistry::new();
    restored.restore(&state);
    assert_eq!(restored.ids("[Image #1] [Image #2]"), vec![1, 2]);
    assert_eq!(restored.add(), "[Image #3]");
}

#[test]
fn prunes_on_install_against_the_current_text() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.add();
    let state = registry.snapshot();

    let mut installed = ImageMarkerRegistry::new();
    installed.install(&state, "keeps [Image #2]");
    assert_eq!(installed.ids("keeps [Image #2]"), vec![2]);
    assert_eq!(installed.snapshot().ids, vec![2]);
    assert_eq!(installed.snapshot().image_counter, 2);
}

#[test]
fn clears_ids_and_the_counter() {
    let mut registry = ImageMarkerRegistry::new();
    registry.add();
    registry.clear();
    assert!(registry.snapshot().ids.is_empty());
    assert_eq!(registry.snapshot().image_counter, 0);
    assert_eq!(registry.add(), "[Image #1]");
}

#[test]
fn returns_an_image_marker_as_one_segment() {
    assert_eq!(
        segments("a[Image #1]b", &["[Image #1]"]),
        vec!["a", "[Image #1]", "b"]
    );
}

#[test]
fn keeps_unauthorized_markers_split_into_graphemes() {
    assert_eq!(segments("[Image #1]", &[]), single_chars("[Image #1]"));
    assert_eq!(
        segments("[Image #2]", &["[Image #1]"]),
        single_chars("[Image #2]")
    );
}

#[test]
fn segments_a_mixed_marker_set_atomically() {
    assert_eq!(
        segments(
            "[Image #1][paste #1 3 chars]!",
            &["[Image #1]", "[paste #1 3 chars]"]
        ),
        vec!["[Image #1]", "[paste #1 3 chars]", "!"]
    );
}

#[test]
fn keeps_segment_with_paste_markers_behavior_for_paste_markers() {
    let base = grapheme_segment_refs("a[paste #1 3 chars]b");
    let valid: BTreeSet<String> = ["[paste #1 3 chars]".to_string()].into_iter().collect();
    let result: Vec<String> = segment_with_paste_markers("a[paste #1 3 chars]b", &base, &valid)
        .into_iter()
        .map(|segment| segment.segment)
        .collect();
    assert_eq!(result, vec!["a", "[paste #1 3 chars]", "b"]);
}

#[test]
fn ignores_paste_markers_that_are_not_authorized() {
    let base = grapheme_segment_refs("a[paste #1 3 chars]b");
    let result: Vec<String> =
        segment_with_paste_markers("a[paste #1 3 chars]b", &base, &BTreeSet::new())
            .into_iter()
            .map(|segment| segment.segment)
            .collect();
    assert_eq!(result, single_chars("a[paste #1 3 chars]b"));
}

#[test]
fn does_not_expand_duplicate_canonical_marker_copies() {
    let (mut editor, _host) = create_editor(30);
    let body = large_paste("SECRET", 12);
    let marker = paste(&mut editor, &body);

    editor.set_text(&format!("{marker} LITERAL-COPY {marker}"));

    assert_eq!(editor.get_expanded_text(), format!("{marker} LITERAL-COPY {marker}"));
}

#[test]
fn does_not_expand_a_queued_prefix_that_duplicates_the_canonical_marker() {
    let (mut editor, _host) = create_editor(30);
    let body = large_paste("QUEUE-BODY", 12);
    let marker = paste(&mut editor, &body);

    editor.set_text(&format!("queued {marker}\n\n{marker}"));

    assert_eq!(editor.get_expanded_text(), format!("queued {marker}\n\n{marker}"));
}

#[test]
fn expands_over_the_original_text_without_recursively_expanding_pasted_bodies() {
    let (mut editor, _host) = create_editor(30);
    let second_body = large_paste("SECOND", 12);
    let second_marker = "[paste #2 +12 lines]";
    let first_body = format!("{second_marker}\n{}", large_paste("FIRST", 11));

    let first_marker = paste(&mut editor, &first_body);
    editor.handle_editor_input(" ");
    paste(&mut editor, &second_body);

    assert_eq!(editor.get_expanded_text(), format!("{first_body} {second_body}"));
    assert!(editor.get_expanded_text().starts_with(second_marker));
    assert_eq!(editor.get_expanded_text().split("SECOND-1\n").count() - 1, 1);
    assert_eq!(first_marker, "[paste #1 +12 lines]");
}

#[test]
fn forward_delete_clears_the_removed_marker_registry_entry() {
    let (mut editor, _host) = create_editor(30);
    let body = large_paste("PRIVATE", 12);
    let marker = paste(&mut editor, &body);

    editor.handle_editor_input("\x01"); // Ctrl+A
    editor.handle_editor_input("\x04"); // Ctrl+D
    assert_eq!(editor.get_text(), "");

    for ch in marker.chars() {
        editor.handle_editor_input(&ch.to_string());
    }

    assert_eq!(editor.get_expanded_text(), marker);
    assert_eq!(editor.get_paste_state().pastes.len(), 0);
}

#[test]
fn kill_and_yank_intentionally_retains_marker_expansion() {
    let (mut editor, _host) = create_editor(30);
    let body = large_paste("YANK", 12);
    paste(&mut editor, &body);

    editor.handle_editor_input("\x01"); // Ctrl+A
    editor.handle_editor_input("\x0b"); // Ctrl+K
    assert_eq!(editor.get_text(), "");
    assert_eq!(editor.get_paste_state().pastes.len(), 1);

    editor.handle_editor_input("\x19"); // Ctrl+Y

    assert_eq!(editor.get_expanded_text(), body);
    assert_eq!(editor.get_paste_state().pastes.len(), 1);
}
