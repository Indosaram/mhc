//! Port of the editor behaviors of senpi `packages/tui/test/editor.test.ts`,
//! `editor-image-marker.test.ts`, `editor-mention-highlight.test.ts`,
//! `editor-history-keybindings.test.ts` and `editor-render-row.test.ts`.

use crate::common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{TestHost, create_editor, create_editor_with_theme, large_paste, paste};
use maho_tui::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSignal,
    AutocompleteSuggestions, MentionRange, SuggestionRequest,
};
use maho_tui::components::editor::{Editor, EditorOptions, EditorTheme};
use maho_tui::components::select_list::SelectListTheme;
use maho_tui::utils::{strip_terminal_sequences, visible_width};

const UNDO: &str = "\x1b[45;5u";
const BACKSPACE: &str = "\x7f";
const FORWARD_DELETE: &str = "\x1b[3~";
const ALT_LEFT: &str = "\x1bb";
const ALT_RIGHT: &str = "\x1bf";
const ARROW_LEFT: &str = "\x1b[D";
const ARROW_RIGHT: &str = "\x1b[C";
const LINE_START: &str = "\x01";
const LINE_END: &str = "\x05";

fn type_text(editor: &mut Editor, text: &str) {
    for ch in text.chars() {
        editor.handle_editor_input(&ch.to_string());
    }
}

fn track_orders(editor: &mut Editor) -> Rc<RefCell<Vec<Vec<u64>>>> {
    let orders = Rc::new(RefCell::new(Vec::new()));
    let sink = orders.clone();
    editor.on_image_markers_changed = Some(Box::new(move |order: &[u64]| {
        sink.borrow_mut().push(order.to_vec());
    }));
    orders
}

// ---------- history ----------

#[test]
fn does_nothing_on_up_arrow_when_history_is_empty() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "");
}

#[test]
fn shows_most_recent_history_entry_on_up_arrow_when_editor_is_empty() {
    let (mut editor, _host) = create_editor(30);
    editor.add_to_history("first prompt");
    editor.add_to_history("second prompt");

    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "second prompt");
}

#[test]
fn cycles_through_history_entries_on_repeated_up_arrow() {
    let (mut editor, _host) = create_editor(30);
    editor.add_to_history("oldest");
    editor.add_to_history("middle");
    editor.add_to_history("newest");

    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "newest");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "middle");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "oldest");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "oldest");
}

#[test]
fn navigates_forward_through_history_with_down_arrow() {
    let (mut editor, _host) = create_editor(30);
    editor.add_to_history("one");
    editor.add_to_history("two");

    editor.handle_editor_input("\x1b[A");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "one");

    editor.handle_editor_input("\x1b[B");
    assert_eq!(editor.get_text(), "two");
    editor.handle_editor_input("\x1b[B");
    assert_eq!(editor.get_text(), "");
}

#[test]
fn exits_history_mode_when_typing_a_character() {
    let (mut editor, _host) = create_editor(30);
    editor.add_to_history("history entry");

    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "history entry");

    editor.handle_editor_input("x");
    assert_eq!(editor.get_text(), "xhistory entry");
}

#[test]
fn exits_history_mode_on_set_text() {
    let (mut editor, _host) = create_editor(30);
    editor.add_to_history("history entry");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "history entry");

    editor.set_text("fresh");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "fresh");
}

#[test]
fn does_not_add_empty_strings_to_history() {
    let (mut editor, _host) = create_editor(30);
    editor.add_to_history("");
    editor.add_to_history("   ");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "");
}

#[test]
fn does_not_add_consecutive_duplicates_to_history() {
    let (mut editor, _host) = create_editor(30);
    editor.add_to_history("same");
    editor.add_to_history("same");

    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "same");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "same");
}

#[test]
fn allows_non_consecutive_duplicates_in_history() {
    let (mut editor, _host) = create_editor(30);
    editor.add_to_history("a");
    editor.add_to_history("b");
    editor.add_to_history("a");

    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "a");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "b");
    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "a");
}

#[test]
fn limits_history_to_100_entries() {
    let (mut editor, _host) = create_editor(30);
    for index in 0..105 {
        editor.add_to_history(&format!("prompt {index}"));
    }

    editor.handle_editor_input("\x1b[A");
    assert_eq!(editor.get_text(), "prompt 104");
    for _ in 0..120 {
        editor.handle_editor_input("\x1b[A");
    }
    assert_eq!(editor.get_text(), "prompt 5");
}

// ---------- basic text ----------

#[test]
fn returns_cursor_position() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("ab\ncd");
    assert_eq!(editor.get_cursor(), (1, 2));
}

#[test]
fn returns_lines_as_a_defensive_copy() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello");
    let mut lines = editor.get_lines();
    lines.push("mutated".to_string());
    assert_eq!(editor.get_lines(), vec!["hello".to_string()]);
}

#[test]
fn inserts_characters_at_the_correct_position_after_cursor_movement_over_umlauts() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "aäb");
    editor.handle_editor_input(ARROW_LEFT);
    type_text(&mut editor, "X");
    assert_eq!(editor.get_text(), "aäXb");
}

#[test]
fn moves_cursor_across_multi_code_unit_emojis_with_single_arrow_key() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "a👍b");
    editor.handle_editor_input(ARROW_LEFT);
    assert_eq!(editor.get_cursor(), (0, "a👍".len()));
    editor.handle_editor_input(ARROW_LEFT);
    assert_eq!(editor.get_cursor(), (0, "a".len()));
}

#[test]
fn deletes_multi_code_unit_emojis_with_single_backspace() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "a👍");
    editor.handle_editor_input(BACKSPACE);
    assert_eq!(editor.get_text(), "a");
}

#[test]
fn replaces_the_entire_document_with_unicode_text_via_set_text() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("Grüße 世界 🎉\nsecond");
    assert_eq!(editor.get_text(), "Grüße 世界 🎉\nsecond");
}

#[test]
fn moves_cursor_to_document_start_on_ctrl_a_and_inserts_at_the_beginning() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello");
    editor.handle_editor_input(LINE_START);
    type_text(&mut editor, "X");
    assert_eq!(editor.get_text(), "Xhello");
}

fn warp_editor() -> Editor {
    let host = TestHost::new(30);
    let env = maho_tui::process_env::env_from(&[
        ("WARP_SESSION_ID", "session"),
        ("WSL_INTEROP", "/run/WSL/123_interop"),
    ]);
    Editor::new(
        host,
        common::plain_editor_theme(),
        EditorOptions {
            terminal_environment: Some(env),
            terminal_platform: Some("linux".to_string()),
            terminal_socket_exists: Some(Rc::new(|_path: &str| true)),
            ..EditorOptions::default()
        },
    )
}

#[test]
fn inserts_lf_as_a_newline_without_submitting_at_the_real_editor_boundary() {
    let mut editor = warp_editor();
    let submitted = Rc::new(RefCell::new(false));
    let sink = submitted.clone();
    editor.on_submit = Some(Box::new(move |_text: &str| {
        *sink.borrow_mut() = true;
    }));

    editor.set_text("hello");
    editor.handle_editor_input("\n");

    assert_eq!(editor.get_text(), "hello\n");
    assert!(!*submitted.borrow());
}

#[test]
fn submits_when_the_terminal_sends_lf_for_plain_enter() {
    let (mut editor, _host) = create_editor(30);
    let submitted = Rc::new(RefCell::new(String::new()));
    let sink = submitted.clone();
    editor.on_submit = Some(Box::new(move |text: &str| {
        *sink.borrow_mut() = text.to_string();
    }));

    editor.set_text("hello from lf enter");
    editor.handle_editor_input("\n");

    assert_eq!(*submitted.borrow(), "hello from lf enter");
    assert_eq!(editor.get_text(), "");
}

#[test]
fn inserts_a_newline_for_shift_enter() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "a");
    editor.handle_editor_input("\x1b[13;2~");
    type_text(&mut editor, "b");
    assert_eq!(editor.get_text(), "a\nb");
}

// ---------- kill ring / yank / undo ----------

#[test]
fn ctrl_w_saves_deleted_text_to_kill_ring_and_ctrl_y_yanks_it() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello world");
    editor.handle_editor_input("\x17"); // Ctrl+W
    assert_eq!(editor.get_text(), "hello ");
    editor.handle_editor_input("\x19"); // Ctrl+Y
    assert_eq!(editor.get_text(), "hello world");
}

#[test]
fn ctrl_u_saves_deleted_text_to_kill_ring() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello");
    editor.handle_editor_input("\x15"); // Ctrl+U
    assert_eq!(editor.get_text(), "");
    editor.handle_editor_input("\x19");
    assert_eq!(editor.get_text(), "hello");
}

#[test]
fn ctrl_k_saves_deleted_text_to_kill_ring() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello world");
    editor.handle_editor_input(LINE_START);
    for _ in 0..6 {
        editor.handle_editor_input(ARROW_RIGHT);
    }
    editor.handle_editor_input("\x0b"); // Ctrl+K
    assert_eq!(editor.get_text(), "hello ");
    editor.handle_editor_input("\x19");
    assert_eq!(editor.get_text(), "hello world");
}

#[test]
fn ctrl_y_does_nothing_when_kill_ring_is_empty() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "abc");
    editor.handle_editor_input("\x19");
    assert_eq!(editor.get_text(), "abc");
}

#[test]
fn alt_y_cycles_through_kill_ring_after_ctrl_y() {
    let (mut editor, _host) = create_editor(30);

    editor.set_text("first");
    editor.handle_editor_input("\x17");
    editor.set_text("second");
    editor.handle_editor_input("\x17");
    editor.set_text("third");
    editor.handle_editor_input("\x17");
    assert_eq!(editor.get_text(), "");

    editor.handle_editor_input("\x19"); // Ctrl+Y - yanks "third"
    assert_eq!(editor.get_text(), "third");
    editor.handle_editor_input("\x1by"); // Alt+Y - cycles to "second"
    assert_eq!(editor.get_text(), "second");
    editor.handle_editor_input("\x1by"); // Alt+Y - cycles to "first"
    assert_eq!(editor.get_text(), "first");
    editor.handle_editor_input("\x1by"); // Alt+Y - cycles back to "third"
    assert_eq!(editor.get_text(), "third");
}

#[test]
fn alt_y_does_nothing_if_not_preceded_by_yank() {
    let (mut editor, _host) = create_editor(30);

    editor.set_text("test");
    editor.handle_editor_input("\x17");
    editor.set_text("other");
    editor.handle_editor_input("x");
    assert_eq!(editor.get_text(), "otherx");

    editor.handle_editor_input("\x1by");
    assert_eq!(editor.get_text(), "otherx");
}

#[test]
fn alt_y_does_nothing_if_kill_ring_has_at_most_one_entry() {
    let (mut editor, _host) = create_editor(30);

    editor.set_text("only");
    editor.handle_editor_input("\x17");
    editor.handle_editor_input("\x19");
    assert_eq!(editor.get_text(), "only");
    editor.handle_editor_input("\x1by");
    assert_eq!(editor.get_text(), "only");
}

#[test]
fn consecutive_ctrl_w_accumulates_into_one_kill_ring_entry() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "one two three");
    editor.handle_editor_input("\x17"); // kill "three"
    editor.handle_editor_input("\x17"); // kill "two"
    editor.handle_editor_input("\x19");
    assert_eq!(editor.get_text(), "one two three");
}

#[test]
fn backward_deletions_prepend_and_forward_deletions_append_during_accumulation() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "abc def");
    editor.handle_editor_input(LINE_START);
    for _ in 0..4 {
        editor.handle_editor_input(ARROW_RIGHT);
    }
    editor.handle_editor_input("\x0b"); // Ctrl+K kills "def"
    editor.handle_editor_input("\x17"); // Ctrl+W kills " " backwards, accumulates as prepend
    editor.handle_editor_input("\x19");
    assert_eq!(editor.get_text(), "abc def");
}

#[test]
fn does_nothing_when_undo_stack_is_empty() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "");
}

#[test]
fn coalesces_consecutive_word_characters_into_one_undo_unit() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "");
}

#[test]
fn undoes_spaces_one_at_a_time() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello  ");
    assert_eq!(editor.get_text(), "hello  ");

    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "hello ");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "hello");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "");
}

#[test]
fn undoes_backspace() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "abc");
    editor.handle_editor_input(BACKSPACE);
    assert_eq!(editor.get_text(), "ab");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "abc");
}

#[test]
fn undoes_forward_delete() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "abc");
    editor.handle_editor_input(LINE_START);
    editor.handle_editor_input(FORWARD_DELETE);
    assert_eq!(editor.get_text(), "bc");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "abc");
}

#[test]
fn undoes_ctrl_w_delete_word_backward() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello world");
    editor.handle_editor_input("\x17");
    assert_eq!(editor.get_text(), "hello ");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "hello world");
}

#[test]
fn undoes_ctrl_k_delete_to_line_end() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello world");
    editor.handle_editor_input(LINE_START);
    for _ in 0..6 {
        editor.handle_editor_input(ARROW_RIGHT);
    }
    editor.handle_editor_input("\x0b");
    assert_eq!(editor.get_text(), "hello ");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "hello world");
}

#[test]
fn undoes_ctrl_u_delete_to_line_start() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello");
    editor.handle_editor_input("\x15");
    assert_eq!(editor.get_text(), "");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "hello");
}

#[test]
fn undoes_yank() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hello world");
    editor.handle_editor_input("\x17");
    editor.handle_editor_input("\x19");
    assert_eq!(editor.get_text(), "hello world");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "hello ");
}

#[test]
fn undo_after_kill_and_yank_restores_the_exact_prior_buffer() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "keep me");
    let before = editor.get_text();
    editor.handle_editor_input("\x0b"); // Ctrl+K kills to end of line
    assert_eq!(editor.get_text(), "keep me");
    editor.handle_editor_input("\x15"); // Ctrl+U kills the rest
    assert_eq!(editor.get_text(), "");
    editor.handle_editor_input("\x19"); // Ctrl+Y yanks
    assert_eq!(editor.get_text(), "keep me");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), before);
}

#[test]
fn undoes_insert_text_at_cursor_atomically() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "abc");
    editor.insert_text_at_cursor("XYZ");
    assert_eq!(editor.get_text(), "abcXYZ");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "abc");
}

#[test]
fn insert_text_at_cursor_handles_multiline_text() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "ab");
    editor.insert_text_at_cursor("X\nY");
    assert_eq!(editor.get_text(), "abX\nY");
}

#[test]
fn insert_text_at_cursor_normalizes_crlf_and_cr_line_endings() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_text_at_cursor("a\r\nb\rc");
    assert_eq!(editor.get_text(), "a\nb\nc");
}

#[test]
fn clears_undo_stack_on_submit() {
    let (mut editor, _host) = create_editor(30);
    let submitted = Rc::new(RefCell::new(Vec::new()));
    let sink = submitted.clone();
    editor.on_submit = Some(Box::new(move |text: &str| {
        sink.borrow_mut().push(text.to_string());
    }));

    type_text(&mut editor, "hello");
    editor.handle_editor_input("\r");
    assert_eq!(submitted.borrow().as_slice(), ["hello"]);
    assert_eq!(editor.get_text(), "");

    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "");
}

#[test]
fn undoes_single_line_paste_atomically() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "abc");
    editor.handle_editor_input("\x1b[200~pasted text\x1b[201~");
    assert_eq!(editor.get_text(), "abcpasted text");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "abc");
}

#[test]
fn undoes_multi_line_paste_atomically() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "abc");
    editor.handle_editor_input("\x1b[200~one\ntwo\x1b[201~");
    assert_eq!(editor.get_text(), "abcone\ntwo");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "abc");
}

#[test]
fn decodes_csi_u_ctrl_letter_sequences_inside_bracketed_paste() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input("\x1b[200~a\x1b[106;5ub\x1b[201~");
    assert_eq!(editor.get_text(), "a\nb");
}

// ---------- paste markers ----------

#[test]
fn creates_a_paste_marker_for_large_pastes() {
    let (mut editor, _host) = create_editor(30);
    let body = large_paste("LINE", 12);
    let marker = paste(&mut editor, &body);
    assert_eq!(marker, "[paste #1 +12 lines]");
    assert_eq!(editor.get_text(), marker);
    assert_eq!(editor.get_expanded_text(), body);
}

#[test]
fn treats_paste_marker_as_single_unit_for_right_and_left_arrow() {
    let (mut editor, _host) = create_editor(30);
    let marker = paste(&mut editor, &large_paste("LINE", 12));

    editor.handle_editor_input(LINE_START);
    editor.handle_editor_input(ARROW_RIGHT);
    assert_eq!(editor.get_cursor(), (0, marker.len()));
    editor.handle_editor_input(ARROW_LEFT);
    assert_eq!(editor.get_cursor(), (0, 0));
}

#[test]
fn treats_paste_marker_as_single_unit_for_backspace() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input("A");
    paste(&mut editor, &large_paste("LINE", 20));
    editor.handle_editor_input("B");
    let text = editor.get_text();
    let marker = text[1..text.len() - 1].to_string();

    editor.handle_editor_input(LINE_START);
    editor.handle_editor_input(ARROW_RIGHT); // past "A"
    editor.handle_editor_input(ARROW_RIGHT); // past marker
    assert_eq!(editor.get_cursor(), (0, 1 + marker.len()));

    editor.handle_editor_input(BACKSPACE);
    assert_eq!(editor.get_text(), "AB");
    assert_eq!(editor.get_cursor(), (0, 1));
}

#[test]
fn treats_paste_marker_as_single_unit_for_forward_delete() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input("A");
    paste(&mut editor, &large_paste("LINE", 20));
    editor.handle_editor_input("B");

    editor.handle_editor_input(LINE_START);
    editor.handle_editor_input(ARROW_RIGHT); // past "A", at start of marker

    editor.handle_editor_input(FORWARD_DELETE);
    assert_eq!(editor.get_text(), "AB");
    assert_eq!(editor.get_cursor(), (0, 1));
}

#[test]
fn treats_paste_marker_as_single_unit_for_word_movement() {
    let (mut editor, _host) = create_editor(30);
    let marker = paste(&mut editor, &large_paste("LINE", 12));

    editor.handle_editor_input(ALT_LEFT);
    assert_eq!(editor.get_cursor(), (0, 0));
    editor.handle_editor_input(ALT_RIGHT);
    assert_eq!(editor.get_cursor(), (0, marker.len()));
}

#[test]
fn undo_restores_marker_after_backspace_deletion() {
    let (mut editor, _host) = create_editor(30);
    let marker = paste(&mut editor, &large_paste("LINE", 12));
    editor.handle_editor_input(BACKSPACE);
    assert_eq!(editor.get_text(), "");
    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), marker);
    assert_eq!(editor.get_expanded_text(), large_paste("LINE", 12));
}

#[test]
fn undo_after_deleting_the_first_of_two_paste_markers_restores_both_registry_entries() {
    let (mut editor, _host) = create_editor(30);
    let body_one = large_paste("FIRST", 12);
    let body_two = large_paste("SECOND", 12);
    paste(&mut editor, &body_one);
    paste(&mut editor, &body_two);
    assert_eq!(
        editor.get_text(),
        "[paste #1 +12 lines][paste #2 +12 lines]"
    );

    editor.handle_editor_input(LINE_START);
    editor.handle_editor_input(FORWARD_DELETE);
    assert_eq!(editor.get_text(), "[paste #1 +12 lines]");
    assert_eq!(editor.get_expanded_text(), body_two);

    editor.handle_editor_input(UNDO);
    assert_eq!(
        editor.get_text(),
        "[paste #1 +12 lines][paste #2 +12 lines]"
    );
    assert_eq!(editor.get_expanded_text(), format!("{body_one}{body_two}"));
}

#[test]
fn set_text_round_trip_preserves_paste_markers_and_registry() {
    let (mut editor, _host) = create_editor(30);
    let body = large_paste("KEEP", 12);
    let marker = paste(&mut editor, &body);

    editor.set_text(&marker);
    assert_eq!(editor.get_text(), marker);
    assert_eq!(editor.get_expanded_text(), body);
}

#[test]
fn set_text_prunes_registry_entries_whose_markers_were_removed() {
    let (mut editor, _host) = create_editor(30);
    let paste_a = large_paste("alpha", 12);
    let paste_b = large_paste("beta", 12);
    paste(&mut editor, &paste_a);
    editor.handle_editor_input(" ");
    let markers = paste(&mut editor, &paste_b);
    let marker_list: Vec<&str> = markers.split("] ").collect();
    assert_eq!(marker_list.len(), 2, "{markers}");

    editor.set_text(marker_list[1]);
    assert_eq!(editor.get_expanded_text(), paste_b);
}

#[test]
fn transfers_paste_registry_to_another_editor_instance() {
    let (mut source, _host) = create_editor(30);
    let body = large_paste("SHARED", 12);
    let marker = paste(&mut source, &body);

    let (mut target, _host2) = create_editor(30);
    target.set_text(&marker);
    target.set_paste_state(&source.get_paste_state());
    assert_eq!(target.get_expanded_text(), body);
}

#[test]
fn set_paste_state_drops_entries_whose_markers_are_not_in_the_current_text() {
    let (mut source, _host) = create_editor(30);
    paste(&mut source, &large_paste("alpha", 12));

    let (mut target, _host2) = create_editor(30);
    target.set_text("no markers here");
    target.set_paste_state(&source.get_paste_state());

    assert_eq!(target.get_expanded_text(), "no markers here");
    target.set_text("");
    let marker = paste(&mut target, &large_paste("beta", 12));
    assert_eq!(marker, "[paste #1 +12 lines]");
}

#[test]
fn set_text_with_unrelated_text_clears_the_registry_and_resets_numbering() {
    let (mut editor, _host) = create_editor(30);
    paste(&mut editor, &large_paste("alpha", 12));
    editor.set_text("plain replacement");
    assert_eq!(editor.get_expanded_text(), "plain replacement");

    editor.set_text("");
    let marker = paste(&mut editor, &large_paste("beta", 12));
    assert_eq!(marker, "[paste #1 +12 lines]");
}

#[test]
fn submits_large_pasted_content_literally() {
    let (mut editor, _host) = create_editor(30);
    let submitted = Rc::new(RefCell::new(String::new()));
    let sink = submitted.clone();
    editor.on_submit = Some(Box::new(move |text: &str| {
        *sink.borrow_mut() = text.to_string();
    }));
    let body = large_paste("BODY", 12);
    paste(&mut editor, &body);
    editor.handle_editor_input("\r");
    assert_eq!(*submitted.borrow(), body);
}

#[test]
fn does_not_treat_manually_typed_marker_like_text_as_atomic() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "[paste #9 5 chars]");
    editor.handle_editor_input(BACKSPACE);
    assert_eq!(editor.get_text(), "[paste #9 5 chars");
}

// ---------- image markers ----------

#[test]
fn inserts_canonical_markers_at_the_cursor_and_returns_increasing_ids() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "look at ");

    assert_eq!(editor.insert_image_marker(), 1);
    assert_eq!(editor.get_text(), "look at [Image #1]");

    type_text(&mut editor, " and ");
    assert_eq!(editor.insert_image_marker(), 2);
    assert_eq!(editor.get_text(), "look at [Image #1] and [Image #2]");
}

#[test]
fn reports_the_marker_order_whenever_markers_change() {
    let (mut editor, _host) = create_editor(30);
    let orders = track_orders(&mut editor);

    editor.insert_image_marker();
    editor.insert_image_marker();

    assert_eq!(*orders.borrow(), vec![vec![1u64], vec![1, 2]]);
}

#[test]
fn renumbers_the_visible_markers_when_one_is_inserted_before_existing_ones() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    editor.handle_editor_input(LINE_START);
    let orders = track_orders(&mut editor);

    let id = editor.insert_image_marker();

    assert_eq!(editor.get_text(), "[Image #1][Image #2]");
    assert_eq!(id, 1);
    assert_eq!(*orders.borrow(), vec![vec![2u64, 1]]);
}

#[test]
fn canonicalizes_the_numbering_after_a_set_text_prune_leaves_a_gap() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    editor.insert_image_marker();
    let orders = track_orders(&mut editor);

    editor.set_text("tail [Image #2]");

    assert_eq!(editor.get_text(), "tail [Image #1]");
    assert_eq!(orders.borrow().last().cloned(), Some(vec![2u64]));
}

#[test]
fn deletes_the_whole_image_marker_with_a_single_backspace() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "a ");
    editor.insert_image_marker();
    let orders = track_orders(&mut editor);

    editor.handle_editor_input(BACKSPACE);

    assert_eq!(editor.get_text(), "a ");
    assert_eq!(*orders.borrow(), vec![Vec::<u64>::new()]);
    assert!(editor.get_image_marker_state().ids.is_empty());
}

#[test]
fn deletes_the_whole_marker_with_forward_delete_and_drops_the_registry_entry() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    type_text(&mut editor, " tail");
    editor.handle_editor_input(LINE_START);
    let orders = track_orders(&mut editor);

    editor.handle_editor_input(FORWARD_DELETE);

    assert_eq!(editor.get_text(), " tail");
    assert_eq!(*orders.borrow(), vec![Vec::<u64>::new()]);
    assert!(editor.get_image_marker_state().ids.is_empty());
}

#[test]
fn renumbers_the_survivor_when_an_earlier_marker_is_deleted() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    editor.insert_image_marker();
    assert_eq!(editor.get_text(), "[Image #1][Image #2]");

    editor.handle_editor_input(LINE_START);
    editor.handle_editor_input(FORWARD_DELETE);
    assert_eq!(editor.get_text(), "[Image #1]");
    assert_eq!(editor.get_image_marker_state().ids, vec![1]);
}

#[test]
fn traverses_the_marker_as_one_unit_with_left_right_arrows() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    editor.handle_editor_input(LINE_START);
    editor.handle_editor_input(ARROW_RIGHT);
    assert_eq!(editor.get_cursor(), (0, "[Image #1]".len()));
    editor.handle_editor_input(ARROW_LEFT);
    assert_eq!(editor.get_cursor(), (0, 0));
}

#[test]
fn never_expands_an_image_marker_in_get_expanded_text() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    assert_eq!(editor.get_expanded_text(), "[Image #1]");
}

#[test]
fn prunes_markers_absent_from_set_text_and_reports_an_empty_order() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    let orders = track_orders(&mut editor);

    editor.set_text("no markers here");
    assert_eq!(editor.get_text(), "no markers here");
    assert_eq!(orders.borrow().last().cloned(), Some(Vec::new()));
}

#[test]
fn keeps_markers_that_survive_set_text() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    editor.insert_image_marker();
    editor.set_text("keep [Image #1] and [Image #2]");
    assert_eq!(editor.get_image_marker_state().ids, vec![1, 2]);
}

#[test]
fn round_trips_image_marker_state_across_editor_instances() {
    let (mut source, _host) = create_editor(30);
    source.insert_image_marker();
    source.insert_image_marker();

    let (mut target, _host2) = create_editor(30);
    target.set_text(&source.get_text());
    target.set_image_marker_state(&source.get_image_marker_state());
    assert_eq!(target.get_image_marker_state().ids, vec![1, 2]);
    assert_eq!(target.insert_image_marker(), 3);
}

#[test]
fn keeps_paste_markers_working_alongside_image_markers() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    let body = large_paste("LINE", 12);
    paste(&mut editor, &body);
    assert_eq!(editor.get_text(), "[Image #1][paste #1 +12 lines]");
    assert_eq!(editor.get_expanded_text(), format!("[Image #1]{body}"));
}

#[test]
fn restores_the_owners_attachment_payloads_when_undoing_a_marker_delete() {
    use std::collections::BTreeMap;

    let (mut editor, _host) = create_editor(30);
    let payloads: Rc<RefCell<BTreeMap<u64, String>>> = Rc::new(RefCell::new(BTreeMap::new()));
    let restored: Rc<RefCell<Vec<BTreeMap<u64, String>>>> = Rc::new(RefCell::new(Vec::new()));
    let snapshot_source = payloads.clone();
    editor.snapshot_attachment_state = Some(Box::new(move || {
        let state: maho_tui::components::editor::AttachmentState =
            Rc::new(snapshot_source.borrow().clone());
        Some(state)
    }));
    let restore_sink = restored.clone();
    editor.restore_attachment_state = Some(Box::new(
        move |state: &maho_tui::components::editor::AttachmentState| {
            let map = state
                .downcast_ref::<BTreeMap<u64, String>>()
                .expect("snapshot is the payload map")
                .clone();
            restore_sink.borrow_mut().push(map);
        },
    ));

    editor.insert_image_marker();
    payloads.borrow_mut().insert(1, "A".to_string());
    editor.insert_image_marker();
    payloads.borrow_mut().insert(2, "B".to_string());
    editor.insert_image_marker();
    payloads.borrow_mut().insert(3, "C".to_string());
    editor.handle_editor_input(ARROW_LEFT);
    editor.handle_editor_input(BACKSPACE);
    assert_eq!(editor.get_text(), "[Image #1][Image #2]");

    editor.handle_editor_input(UNDO);

    assert_eq!(editor.get_text(), "[Image #1][Image #2][Image #3]");
    let last = restored
        .borrow()
        .last()
        .cloned()
        .expect("undo restored the payloads");
    assert_eq!(
        last.into_iter().collect::<Vec<_>>(),
        vec![
            (1, "A".to_string()),
            (2, "B".to_string()),
            (3, "C".to_string()),
        ]
    );
}

#[test]
fn renumbers_the_survivor_when_a_later_marker_is_backspaced() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    type_text(&mut editor, " ");
    editor.insert_image_marker();
    let orders = track_orders(&mut editor);

    editor.handle_editor_input(BACKSPACE);

    assert_eq!(editor.get_text(), "[Image #1] ");
    assert_eq!(*orders.borrow(), vec![vec![1u64]]);
}

#[test]
fn crosses_the_marker_as_one_unit_for_alt_left_alt_right_word_navigation() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "aa ");
    editor.insert_image_marker();
    type_text(&mut editor, " bb");
    assert_eq!(editor.get_text(), "aa [Image #1] bb");

    editor.handle_editor_input(LINE_END);
    editor.handle_editor_input(ALT_LEFT);
    assert_eq!(editor.get_cursor().1, 14);
    editor.handle_editor_input(ALT_LEFT);
    assert_eq!(editor.get_cursor().1, 3);

    editor.handle_editor_input(ALT_RIGHT);
    assert_eq!(editor.get_cursor().1, 13);
}

fn content_lines(editor: &mut Editor, width: usize) -> Vec<String> {
    let lines = editor.render_editor(width);
    lines[1..lines.len() - 1]
        .iter()
        .map(|line| strip_terminal_sequences(line).trim_end().to_string())
        .collect()
}

#[test]
fn wraps_the_marker_as_one_atomic_segment_and_never_breaks_at_its_inner_space() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "ab ");
    editor.insert_image_marker();
    type_text(&mut editor, " cd");
    assert_eq!(editor.get_text(), "ab [Image #1] cd");

    // 13 usable columns + 1 reserved for the cursor.
    assert_eq!(content_lines(&mut editor, 14), vec!["ab", "[Image #1] cd"]);
}

#[test]
fn wraps_the_marker_onto_its_own_visual_line_when_it_does_not_fit() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "ab ");
    editor.insert_image_marker();
    type_text(&mut editor, " cd");

    // 10 usable columns + 1 reserved for the cursor.
    assert_eq!(
        content_lines(&mut editor, 11),
        vec!["ab", "[Image #1]", " cd"]
    );
}

#[test]
fn restores_marker_text_and_registry_entry_when_undoing_a_delete() {
    let (mut editor, _host) = create_editor(30);
    editor.insert_image_marker();
    editor.handle_editor_input(BACKSPACE);
    assert_eq!(editor.get_text(), "");
    let orders = track_orders(&mut editor);

    editor.handle_editor_input(UNDO);

    assert_eq!(editor.get_text(), "[Image #1]");
    assert_eq!(editor.get_image_marker_state().ids, vec![1]);
    assert_eq!(*orders.borrow(), vec![vec![1u64]]);
}

#[test]
fn drops_the_registry_entry_when_undoing_an_insert() {
    let (mut editor, _host) = create_editor(30);
    type_text(&mut editor, "hi");
    editor.insert_image_marker();
    let orders = track_orders(&mut editor);

    editor.handle_editor_input(UNDO);

    assert_eq!(editor.get_text(), "hi");
    assert!(editor.get_image_marker_state().ids.is_empty());
    assert_eq!(*orders.borrow(), vec![Vec::<u64>::new()]);
}

// ---------- backslash+Enter newline workaround ----------

fn submit_flag(editor: &mut Editor) -> Rc<RefCell<bool>> {
    let submitted = Rc::new(RefCell::new(false));
    let sink = submitted.clone();
    editor.on_submit = Some(Box::new(move |_text: &str| {
        *sink.borrow_mut() = true;
    }));
    submitted
}

#[test]
fn inserts_newline_for_backslash_followed_by_lf_enter() {
    let (mut editor, _host) = create_editor(30);
    let submitted = submit_flag(&mut editor);

    editor.handle_editor_input("\\");
    editor.handle_editor_input("\n");

    assert_eq!(editor.get_text(), "\n");
    assert!(!*submitted.borrow());
}

#[test]
fn inserts_backslash_immediately_without_buffering() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input("\\");
    assert_eq!(editor.get_text(), "\\");
}

#[test]
fn converts_standalone_backslash_to_newline_on_enter() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input("\\");
    editor.handle_editor_input("\r");
    assert_eq!(editor.get_text(), "\n");
}

#[test]
fn inserts_backslash_normally_when_followed_by_other_characters() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input("\\");
    editor.handle_editor_input("x");
    assert_eq!(editor.get_text(), "\\x");
}

#[test]
fn does_not_trigger_newline_when_backslash_is_not_immediately_before_cursor() {
    let (mut editor, _host) = create_editor(30);
    let submitted = submit_flag(&mut editor);

    editor.handle_editor_input("\\");
    editor.handle_editor_input("x");
    editor.handle_editor_input("\r");

    assert!(*submitted.borrow());
}

#[test]
fn only_removes_one_backslash_when_multiple_are_present() {
    let (mut editor, _host) = create_editor(30);
    editor.handle_editor_input("\\");
    editor.handle_editor_input("\\");
    editor.handle_editor_input("\\");
    assert_eq!(editor.get_text(), "\\\\\\");

    editor.handle_editor_input("\r");
    assert_eq!(editor.get_text(), "\\\\\n");
}

// ---------- character jump (Ctrl+]) ----------

const JUMP_FORWARD: &str = "\x1d";
const JUMP_BACKWARD: &str = "\x1b\x1d";

#[test]
fn jumps_forward_to_first_occurrence_of_character_on_same_line() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");
    editor.handle_editor_input(LINE_START);
    assert_eq!(editor.get_cursor(), (0, 0));

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("o");

    assert_eq!(editor.get_cursor(), (0, 4));
}

#[test]
fn jumps_forward_to_next_occurrence_after_cursor() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");
    editor.handle_editor_input(LINE_START);
    for _ in 0..4 {
        editor.handle_editor_input(ARROW_RIGHT);
    }
    assert_eq!(editor.get_cursor(), (0, 4));

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("o");

    assert_eq!(editor.get_cursor(), (0, 7));
}

#[test]
fn jumps_forward_across_multiple_lines() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("abc\ndef\nghi");
    editor.handle_editor_input("\x1b[A");
    editor.handle_editor_input("\x1b[A");
    editor.handle_editor_input(LINE_START);
    assert_eq!(editor.get_cursor(), (0, 0));

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("g");

    assert_eq!(editor.get_cursor(), (2, 0));
}

#[test]
fn jumps_backward_to_first_occurrence_before_cursor_on_same_line() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");
    assert_eq!(editor.get_cursor(), (0, 11));

    editor.handle_editor_input(JUMP_BACKWARD);
    editor.handle_editor_input("o");

    assert_eq!(editor.get_cursor(), (0, 7));
}

#[test]
fn jumps_backward_across_multiple_lines() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("abc\ndef\nghi");
    assert_eq!(editor.get_cursor(), (2, 3));

    editor.handle_editor_input(JUMP_BACKWARD);
    editor.handle_editor_input("a");

    assert_eq!(editor.get_cursor(), (0, 0));
}

#[test]
fn does_nothing_when_character_is_not_found_forward() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");
    editor.handle_editor_input(LINE_START);

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("z");

    assert_eq!(editor.get_cursor(), (0, 0));
}

#[test]
fn does_nothing_when_character_is_not_found_backward() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");
    assert_eq!(editor.get_cursor(), (0, 11));

    editor.handle_editor_input(JUMP_BACKWARD);
    editor.handle_editor_input("z");

    assert_eq!(editor.get_cursor(), (0, 11));
}

#[test]
fn jump_is_case_sensitive() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("Hello World");
    editor.handle_editor_input(LINE_START);

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("h");
    assert_eq!(editor.get_cursor(), (0, 0));

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("W");
    assert_eq!(editor.get_cursor(), (0, 6));
}

#[test]
fn cancels_jump_mode_when_ctrl_bracket_is_pressed_again() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");
    editor.handle_editor_input(LINE_START);

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input(JUMP_FORWARD);

    editor.handle_editor_input("o");
    assert_eq!(editor.get_text(), "ohello world");
}

#[test]
fn cancels_jump_mode_on_escape_and_processes_the_escape() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");
    editor.handle_editor_input(LINE_START);

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("\x1b");
    assert_eq!(editor.get_cursor(), (0, 0));

    editor.handle_editor_input("o");
    assert_eq!(editor.get_text(), "ohello world");
}

#[test]
fn cancels_backward_jump_mode_when_ctrl_alt_bracket_is_pressed_again() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");

    editor.handle_editor_input(JUMP_BACKWARD);
    editor.handle_editor_input(JUMP_BACKWARD);

    editor.handle_editor_input("o");
    assert_eq!(editor.get_text(), "hello worldo");
}

#[test]
fn jump_searches_for_special_characters() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("foo(bar) = baz;");
    editor.handle_editor_input(LINE_START);

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("(");
    assert_eq!(editor.get_cursor(), (0, 3));

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("=");
    assert_eq!(editor.get_cursor(), (0, 9));
}

#[test]
fn jump_handles_empty_text_gracefully() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("");
    assert_eq!(editor.get_cursor(), (0, 0));

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("x");

    assert_eq!(editor.get_cursor(), (0, 0));
}

#[test]
fn resets_last_action_when_jumping() {
    let (mut editor, _host) = create_editor(30);
    editor.set_text("hello world");
    editor.handle_editor_input(LINE_START);

    editor.handle_editor_input("x");
    assert_eq!(editor.get_text(), "xhello world");

    editor.handle_editor_input(JUMP_FORWARD);
    editor.handle_editor_input("o");

    editor.handle_editor_input("Y");
    assert_eq!(editor.get_text(), "xhellYo world");

    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "xhello world");
}

// ---------- mention highlight ----------

fn mention_provider() -> Rc<RefCell<dyn AutocompleteProvider>> {
    struct MentionProvider;
    impl AutocompleteProvider for MentionProvider {
        fn get_suggestions(
            &mut self,
            _lines: &[String],
            _cursor_line: usize,
            _cursor_col: usize,
            _force: bool,
        ) -> Option<AutocompleteSuggestions> {
            None
        }

        fn apply_completion(
            &self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            _item: &AutocompleteItem,
            _prefix: &str,
        ) -> ApplyCompletionResult {
            ApplyCompletionResult {
                lines: lines.to_vec(),
                cursor_line,
                cursor_col,
            }
        }

        fn get_mention_ranges(&self, line: &str) -> Vec<MentionRange> {
            let mut ranges = Vec::new();
            let mut offset = 0usize;
            while let Some(index) = line[offset..].find("$debugging") {
                let start = offset + index;
                ranges.push(MentionRange {
                    start,
                    end: start + "$debugging".len(),
                });
                offset = start + "$debugging".len();
            }
            ranges
        }
    }
    Rc::new(RefCell::new(MentionProvider))
}

const BLUE_BOLD: &str = "\x1b[1;34m";
const RESET: &str = "\x1b[22;39m";

fn styled(text: &str) -> String {
    format!("{BLUE_BOLD}{text}{RESET}")
}

fn mention_theme() -> EditorTheme {
    EditorTheme {
        border_color: Rc::new(|text: &str| text.to_string()),
        mention: Some(Rc::new(|text: &str| format!("{BLUE_BOLD}{text}{RESET}"))),
        select_list: common::plain_select_list_theme(),
    }
}

fn create_mention_editor(theme: EditorTheme, rows: usize) -> (Editor, Rc<TestHost>) {
    let (mut editor, host) = create_editor_with_theme(theme, rows);
    editor.set_autocomplete_provider(mention_provider());
    (editor, host)
}

#[test]
fn styles_every_mention_range_and_leaves_the_rest_of_the_line_plain() {
    let (mut editor, _host) = create_mention_editor(mention_theme(), 24);
    editor.set_text("fix $debugging then $HOME $debugging");

    let lines = editor.render_editor(80);
    let line = lines.get(1).cloned().unwrap_or_default();

    let expected = format!(
        "fix {} then $HOME {}",
        styled("$debugging"),
        styled("$debugging")
    );
    assert!(line.contains(&expected), "{line:?}");
    assert_eq!(visible_width(&line), 80);
}

#[test]
fn keeps_both_halves_styled_when_the_cursor_sits_inside_a_mention() {
    let (mut editor, _host) = create_mention_editor(mention_theme(), 24);
    editor.set_text("$debugging");
    for _ in 0..4 {
        editor.handle_editor_input(ARROW_LEFT);
    }

    let lines = editor.render_editor(80);
    let line = lines.get(1).cloned().unwrap_or_default();

    let expected = format!("{}\x1b[7mg\x1b[0m{}", styled("$debug"), styled("ing"));
    assert!(line.contains(&expected), "{line:?}");
}

#[test]
fn styles_a_mention_on_every_logical_line_and_across_wrap_chunks() {
    let (mut editor, _host) = create_mention_editor(mention_theme(), 24);
    editor.set_text("first\nuse $debugging");

    let lines = editor.render_editor(12);
    assert!(
        lines
            .iter()
            .any(|line| line.contains(&styled("$debugging"))),
        "{lines:?}"
    );
}

#[test]
fn renders_plain_text_when_the_theme_has_no_mention_style() {
    let (mut editor, _host) = create_mention_editor(common::plain_editor_theme(), 24);
    editor.set_text("fix $debugging");

    let lines = editor.render_editor(80);
    let line = lines.get(1).cloned().unwrap_or_default();

    assert!(line.contains("fix $debugging"), "{line:?}");
    assert!(!line.contains(BLUE_BOLD), "{line:?}");
}

// ---------- select-list renderRow threading ----------

fn slash_provider() -> Rc<RefCell<dyn AutocompleteProvider>> {
    struct SlashProvider;
    impl AutocompleteProvider for SlashProvider {
        fn get_suggestions(
            &mut self,
            lines: &[String],
            _cursor_line: usize,
            cursor_col: usize,
            _force: bool,
        ) -> Option<AutocompleteSuggestions> {
            let before: String = lines
                .first()
                .map(|line| line.chars().take(cursor_col).collect())
                .unwrap_or_default();
            if !before.starts_with('/') {
                return None;
            }
            Some(AutocompleteSuggestions {
                items: vec![
                    AutocompleteItem {
                        value: "/help".to_string(),
                        label: "/help".to_string(),
                        description: Some("Show help".to_string()),
                    },
                    AutocompleteItem {
                        value: "/quit".to_string(),
                        label: "/quit".to_string(),
                        description: Some("Exit the app".to_string()),
                    },
                ],
                prefix: before,
            })
        }

        fn apply_completion(
            &self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            item: &AutocompleteItem,
            prefix: &str,
        ) -> ApplyCompletionResult {
            let line = lines.get(cursor_line).cloned().unwrap_or_default();
            let mut new_lines = lines.to_vec();
            let before = line
                .get(..cursor_col.saturating_sub(prefix.len()))
                .unwrap_or("")
                .to_string();
            let after = line.get(cursor_col..).unwrap_or("").to_string();
            new_lines[cursor_line] = format!("{before}{}{after}", item.value);
            ApplyCompletionResult {
                lines: new_lines,
                cursor_line,
                cursor_col: cursor_col - prefix.len() + item.value.len(),
            }
        }
    }
    Rc::new(RefCell::new(SlashProvider))
}

#[test]
fn routes_the_editor_themes_select_list_render_row_into_the_slash_autocomplete_list() {
    let theme = EditorTheme {
        border_color: Rc::new(|text: &str| text.to_string()),
        mention: None,
        select_list: SelectListTheme {
            selected_prefix: Rc::new(|text: &str| format!("[BLUE]{text}[/BLUE]")),
            selected_text: Rc::new(|text: &str| format!("[S]{text}[/S]")),
            description: Rc::new(|text: &str| format!("[D]{text}[/D]")),
            scroll_info: Rc::new(|text: &str| format!("[I]{text}[/I]")),
            no_match: Rc::new(|text: &str| format!("[N]{text}[/N]")),
            render_row: Some(Rc::new(
                |parts: &maho_tui::components::select_list::SelectListRowParts| {
                    let description = parts.description.clone().unwrap_or_default();
                    if parts.is_selected {
                        format!("[BG]{}{}{}[/BG]", parts.prefix, parts.primary, description)
                    } else {
                        format!("{}{}{}", parts.prefix, parts.primary, description)
                    }
                },
            )),
        },
    };
    let (mut editor, _host) = create_editor_with_theme(theme, 24);
    editor.set_autocomplete_provider(slash_provider());

    editor.handle_editor_input("/");
    assert!(editor.is_showing_autocomplete());

    let rendered = strip_terminal_sequences(&editor.render_editor(80).join("\n"));
    let expected_selected = format!("[BG][BLUE]→ [/BLUE]/help{}Show help[/BG]", " ".repeat(7));
    assert!(
        rendered.contains(&expected_selected),
        "expected custom slash row in editor render, got:\n{rendered}"
    );
    let expected_unselected = format!("  /quit{}Exit the app", " ".repeat(7));
    assert!(
        rendered.contains(&expected_unselected),
        "unselected row should use renderRow too"
    );
}

#[test]
fn keeps_a_plain_editor_building_with_default_options() {
    let host = TestHost::new(30);
    let editor = Editor::new(host, common::plain_editor_theme(), EditorOptions::default());
    assert_eq!(editor.get_text(), "");
}

// ---------- asynchronous `@` autocomplete: debounce, cancellation ----------

/// Requests answered later through a channel the test resolves, like senpi's awaited fd walk.
#[derive(Default)]
struct DeferredRequests {
    requests: Vec<(
        AutocompleteSignal,
        std::sync::mpsc::Sender<Option<AutocompleteSuggestions>>,
    )>,
}

fn deferred_provider(
    state: Rc<RefCell<DeferredRequests>>,
) -> Rc<RefCell<dyn AutocompleteProvider>> {
    struct DeferredProvider(Rc<RefCell<DeferredRequests>>);
    impl AutocompleteProvider for DeferredProvider {
        fn get_suggestions(
            &mut self,
            _: &[String],
            _: usize,
            _: usize,
            _: bool,
        ) -> Option<AutocompleteSuggestions> {
            None
        }

        fn request_suggestions(
            &mut self,
            _lines: &[String],
            _cursor_line: usize,
            _cursor_col: usize,
            _force: bool,
            signal: &AutocompleteSignal,
        ) -> SuggestionRequest {
            let (sender, receiver) = std::sync::mpsc::channel();
            self.0.borrow_mut().requests.push((signal.clone(), sender));
            SuggestionRequest::Pending(receiver)
        }

        fn apply_completion(
            &self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            _item: &AutocompleteItem,
            _prefix: &str,
        ) -> ApplyCompletionResult {
            ApplyCompletionResult {
                lines: lines.to_vec(),
                cursor_line,
                cursor_col,
            }
        }
    }
    Rc::new(RefCell::new(DeferredProvider(state)))
}

fn file_page(prefix: &str) -> Option<AutocompleteSuggestions> {
    Some(AutocompleteSuggestions {
        items: vec![AutocompleteItem {
            value: "@src/main.rs".to_string(),
            label: "main.rs".to_string(),
            description: Some("src/main.rs".to_string()),
        }],
        prefix: prefix.to_string(),
    })
}

fn after_debounce() -> std::time::Instant {
    std::time::Instant::now() + std::time::Duration::from_secs(1)
}

#[test]
fn debounces_an_at_trigger_until_the_host_polls_past_the_deadline() {
    let (mut editor, _host) = create_editor(30);
    let state = Rc::new(RefCell::new(DeferredRequests::default()));
    editor.set_autocomplete_provider(deferred_provider(state.clone()));

    editor.handle_editor_input("@");
    assert_eq!(
        state.borrow().requests.len(),
        0,
        "the @ request waits for the debounce"
    );

    assert!(
        editor.poll_autocomplete_at(after_debounce()),
        "the request is in flight"
    );
    assert_eq!(state.borrow().requests.len(), 1);
    assert!(!editor.is_showing_autocomplete());

    state.borrow().requests[0]
        .1
        .send(file_page("@"))
        .expect("resolve request");
    assert!(!editor.poll_autocomplete_at(after_debounce()));
    assert!(editor.is_showing_autocomplete());
}

#[test]
fn aborts_the_in_flight_request_and_ignores_its_stale_answer_after_another_keystroke() {
    let (mut editor, _host) = create_editor(30);
    let state = Rc::new(RefCell::new(DeferredRequests::default()));
    editor.set_autocomplete_provider(deferred_provider(state.clone()));

    editor.handle_editor_input("@");
    editor.poll_autocomplete_at(after_debounce());
    editor.handle_editor_input("s");
    assert!(
        state.borrow().requests[0].0.is_aborted(),
        "a newer keystroke aborts the walk"
    );

    // The superseded request's receiver is gone, so its late answer cannot reach the editor.
    assert!(state.borrow().requests[0].1.send(file_page("@")).is_err());
    editor.poll_autocomplete_at(after_debounce());
    assert_eq!(state.borrow().requests.len(), 2);
    assert!(!editor.is_showing_autocomplete());

    state.borrow().requests[1]
        .1
        .send(file_page("@s"))
        .expect("resolve request");
    editor.poll_autocomplete_at(after_debounce());
    assert!(editor.is_showing_autocomplete());
}

// ---------- editor-history-keybindings.test.ts ----------

/// Restores the default global keybindings when the test ends (senpi's `afterEach`).
struct DefaultKeybindingsOnDrop;

impl Drop for DefaultKeybindingsOnDrop {
    fn drop(&mut self) {
        maho_tui::keybindings::set_keybindings(maho_tui::keybindings::KeybindingsManager::new(
            maho_tui::keybindings::tui_keybindings(),
            Default::default(),
        ));
    }
}

/// Only this test sends Ctrl+P/Ctrl+N, which are unbound by default, so installing the global
/// bindings cannot change another test's keystrokes.
#[test]
fn browses_history_directly_without_first_moving_the_cursor() {
    use maho_tui::keybindings::{KeybindingsManager, Keys, set_keybindings, tui_keybindings};

    let _lock = common::global_keybindings_lock();
    let _restore = DefaultKeybindingsOnDrop;
    let mut user_bindings = maho_tui::keybindings::KeybindingsConfig::new();
    user_bindings.insert(
        "tui.editor.historyPrevious".to_string(),
        Some(Keys::from("ctrl+p")),
    );
    user_bindings.insert(
        "tui.editor.historyNext".to_string(),
        Some(Keys::from("ctrl+n")),
    );
    set_keybindings(KeybindingsManager::new(tui_keybindings(), user_bindings));

    let (mut editor, _host) = create_editor(24);
    editor.add_to_history("older prompt");
    editor.add_to_history("newer\nmultiline prompt");
    editor.set_text("draft");
    editor.handle_editor_input(ARROW_LEFT);
    editor.handle_editor_input(ARROW_LEFT);

    editor.handle_editor_input("\x10");
    assert_eq!(editor.get_text(), "newer\nmultiline prompt");
    assert_eq!(editor.get_cursor(), (0, 0));

    editor.handle_editor_input("\x10");
    assert_eq!(editor.get_text(), "older prompt");

    editor.handle_editor_input("\x0e");
    assert_eq!(editor.get_text(), "newer\nmultiline prompt");
    assert_eq!(editor.get_cursor(), (1, 16));

    editor.handle_editor_input("\x0e");
    assert_eq!(editor.get_text(), "draft");
    assert_eq!(editor.get_cursor(), (0, 3));
}

// ---------- editor-dollar-autocomplete.test.ts ----------

/// senpi's awaited provider promises become channels the test resolves; the debounce timer and
/// the promise continuation are driven with `poll_autocomplete_at`.
mod editor_dollar_autocomplete {
    use super::*;
    use maho_tui::autocomplete::{CombinedAutocompleteProvider, CommandSpec};

    type Request = (String, usize, usize);

    struct RecordingProvider {
        requests: Rc<RefCell<Vec<Request>>>,
        answers: Rc<RefCell<Vec<std::sync::mpsc::Sender<Option<AutocompleteSuggestions>>>>>,
        immediate: Option<AutocompleteSuggestions>,
    }

    impl AutocompleteProvider for RecordingProvider {
        fn get_suggestions(
            &mut self,
            _: &[String],
            _: usize,
            _: usize,
            _: bool,
        ) -> Option<AutocompleteSuggestions> {
            None
        }

        fn request_suggestions(
            &mut self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            _force: bool,
            _signal: &AutocompleteSignal,
        ) -> SuggestionRequest {
            self.requests.borrow_mut().push((
                lines.get(cursor_line).cloned().unwrap_or_default(),
                cursor_line,
                cursor_col,
            ));
            if let Some(answer) = self.immediate.clone() {
                return SuggestionRequest::Ready(Some(answer));
            }
            let (sender, receiver) = std::sync::mpsc::channel();
            self.answers.borrow_mut().push(sender);
            SuggestionRequest::Pending(receiver)
        }

        fn apply_completion(
            &self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            _item: &AutocompleteItem,
            _prefix: &str,
        ) -> ApplyCompletionResult {
            ApplyCompletionResult {
                lines: lines.to_vec(),
                cursor_line,
                cursor_col,
            }
        }
    }

    fn debugging(prefix: &str) -> AutocompleteSuggestions {
        AutocompleteSuggestions {
            items: vec![AutocompleteItem {
                value: "$debugging".to_string(),
                label: "$debugging".to_string(),
                description: Some("Debug runtime failures".to_string()),
            }],
            prefix: prefix.to_string(),
        }
    }

    fn after_debounce() -> std::time::Instant {
        std::time::Instant::now() + std::time::Duration::from_secs(1)
    }

    fn editor_with(
        immediate: Option<AutocompleteSuggestions>,
    ) -> (
        Editor,
        Rc<RefCell<Vec<Request>>>,
        Rc<RefCell<Vec<std::sync::mpsc::Sender<Option<AutocompleteSuggestions>>>>>,
    ) {
        let (mut editor, _host) = create_editor(24);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let answers = Rc::new(RefCell::new(Vec::new()));
        editor.set_autocomplete_provider(Rc::new(RefCell::new(RecordingProvider {
            requests: Rc::clone(&requests),
            answers: Rc::clone(&answers),
            immediate,
        })));
        (editor, requests, answers)
    }

    #[test]
    fn requests_and_renders_suggestions_when_the_prompt_starts_with_dollar() {
        let (mut editor, requests, _answers) = editor_with(Some(debugging("$deb")));
        type_text(&mut editor, "$deb");
        editor.poll_autocomplete_at(after_debounce());

        assert_eq!(
            requests.borrow().last().cloned(),
            Some(("$deb".to_string(), 0, 4))
        );
        assert!(editor.render_editor(80).join("\n").contains("$debugging"));
    }

    #[test]
    fn requests_dollar_autocomplete_on_a_later_logical_line() {
        let (mut editor, requests, _answers) = editor_with(Some(debugging("$")));
        type_text(&mut editor, "first line");
        editor.handle_editor_input("\x1b[13;2u");
        type_text(&mut editor, "text $");
        editor.poll_autocomplete_at(after_debounce());

        assert_eq!(
            requests.borrow().last().cloned(),
            Some(("text $".to_string(), 1, 6))
        );
        assert!(editor.render_editor(80).join("\n").contains("$debugging"));
    }

    #[test]
    fn reopens_dollar_autocomplete_after_a_multiline_paste_and_follow_up_typing() {
        let provider = CombinedAutocompleteProvider::new(
            vec![CommandSpec::with_description(
                "skill:debugging",
                "Debug runtime failures",
            )],
            "/tmp",
            None,
        );
        let (mut editor, _host) = create_editor(24);
        editor.set_autocomplete_provider(Rc::new(RefCell::new(provider)));

        editor.handle_editor_input("\x1b[200~first line\ntext $\x1b[201~");
        editor.handle_editor_input("d");
        editor.poll_autocomplete_at(after_debounce());

        assert_eq!(editor.get_text(), "first line\ntext $d");
        assert!(editor.is_showing_autocomplete());
    }

    #[test]
    fn keeps_dollar_autocomplete_responsive_while_a_previous_request_is_pending() {
        let (mut editor, requests, answers) = editor_with(None);
        editor.handle_editor_input("$");
        editor.poll_autocomplete_at(after_debounce());
        assert_eq!(requests.borrow().len(), 1);

        editor.handle_editor_input("d");
        // The first request is superseded; its late answer cannot reach the editor.
        let _ = answers.borrow()[0].send(None);
        editor.poll_autocomplete_at(after_debounce());
        assert_eq!(requests.borrow().len(), 2);
        answers.borrow()[1]
            .send(Some(debugging("$d")))
            .expect("resolve the second request");
        editor.poll_autocomplete_at(after_debounce());

        assert_eq!(editor.get_text(), "$d");
        assert_eq!(requests.borrow().len(), 2);
    }
}

// senpi editor.test.ts "Sticky column" (preferredVisualCol).
mod sticky_column {
    use super::*;
    use maho_tui::tui::Component;

    const UP: &str = "\x1b[A";
    const DOWN: &str = "\x1b[B";
    const CTRL_LEFT: &str = "\x1b[1;5D";
    const CTRL_RIGHT: &str = "\x1b[1;5C";

    fn editor_with(text: &str) -> Editor {
        let (mut editor, _host) = create_editor(24);
        editor.set_text(text);
        editor
    }

    fn press(editor: &mut Editor, key: &str, times: usize) {
        for _ in 0..times {
            editor.handle_editor_input(key);
        }
    }

    /// senpi `positionCursor`: top line, down `line` lines, line start, right `col` columns.
    fn position_cursor(editor: &mut Editor, line: usize, col: usize) {
        press(editor, UP, 20);
        press(editor, DOWN, line);
        editor.handle_editor_input(LINE_START);
        press(editor, ARROW_RIGHT, col);
    }

    #[test]
    fn preserves_target_column_when_moving_up_through_a_shorter_line() {
        let mut editor = editor_with("2222222222x222\n\n1111111111_111111111111");
        assert_eq!(editor.get_cursor(), (2, 23));
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 10);
        assert_eq!(editor.get_cursor(), (2, 10));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor(), (1, 0));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor(), (0, 10));
    }

    #[test]
    fn preserves_target_column_when_moving_down_through_a_shorter_line() {
        let mut editor = editor_with("1111111111_111\n\n2222222222x222222222222");
        press(&mut editor, UP, 2);
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 10);
        assert_eq!(editor.get_cursor(), (0, 10));
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_cursor(), (1, 0));
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_cursor(), (2, 10));
    }

    #[test]
    fn resets_sticky_column_on_horizontal_movement_left_arrow() {
        let mut editor = editor_with("1234567890\n\n1234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 5);
        assert_eq!(editor.get_cursor(), (2, 5));
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 5));
        editor.handle_editor_input(ARROW_LEFT);
        assert_eq!(editor.get_cursor(), (0, 4));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 4));
    }

    #[test]
    fn resets_sticky_column_on_horizontal_movement_right_arrow() {
        let mut editor = editor_with("1234567890\n\n1234567890");
        press(&mut editor, UP, 2);
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 5);
        assert_eq!(editor.get_cursor(), (0, 5));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 5));
        editor.handle_editor_input(ARROW_RIGHT);
        assert_eq!(editor.get_cursor(), (2, 6));
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 6));
    }

    #[test]
    fn resets_sticky_column_on_typing() {
        let mut editor = editor_with("1234567890\n\n1234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 8);
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 8));
        editor.handle_editor_input("X");
        assert_eq!(editor.get_cursor(), (0, 9));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 9));
    }

    #[test]
    fn resets_sticky_column_on_backspace() {
        let mut editor = editor_with("1234567890\n\n1234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 8);
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 8));
        editor.handle_editor_input(BACKSPACE);
        assert_eq!(editor.get_cursor(), (0, 7));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 7));
    }

    #[test]
    fn resets_sticky_column_on_ctrl_a_move_to_line_start() {
        let mut editor = editor_with("1234567890\n\n1234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 8);
        editor.handle_editor_input(UP);
        editor.handle_editor_input(LINE_START);
        assert_eq!(editor.get_cursor(), (1, 0));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor(), (0, 0));
    }

    #[test]
    fn resets_sticky_column_on_ctrl_e_move_to_line_end() {
        let mut editor = editor_with("12345\n\n1234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 3);
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 3));
        editor.handle_editor_input(LINE_END);
        assert_eq!(editor.get_cursor(), (0, 5));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 5));
    }

    #[test]
    fn resets_sticky_column_on_word_movement_ctrl_left() {
        let mut editor = editor_with("hello world\n\nhello world");
        assert_eq!(editor.get_cursor(), (2, 11));
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 11));
        editor.handle_editor_input(CTRL_LEFT);
        assert_eq!(editor.get_cursor(), (0, 6));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 6));
    }

    #[test]
    fn resets_sticky_column_on_word_movement_ctrl_right() {
        let mut editor = editor_with("hello world\n\nhello world");
        press(&mut editor, UP, 2);
        editor.handle_editor_input(LINE_START);
        assert_eq!(editor.get_cursor(), (0, 0));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 0));
        editor.handle_editor_input(CTRL_RIGHT);
        assert_eq!(editor.get_cursor(), (2, 5));
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 5));
    }

    #[test]
    fn resets_sticky_column_on_undo() {
        let mut editor = editor_with("1234567890\n\n1234567890");
        press(&mut editor, UP, 2);
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 8);
        assert_eq!(editor.get_cursor(), (0, 8));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 8));
        editor.handle_editor_input("X");
        assert_eq!(editor.get_text(), "1234567890\n\n12345678X90");
        assert_eq!(editor.get_cursor(), (2, 9));
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 9));
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "1234567890\n\n1234567890");
        assert_eq!(editor.get_cursor(), (2, 8));
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 8));
    }

    #[test]
    fn handles_multiple_consecutive_up_down_movements() {
        let mut editor = editor_with("1234567890\nab\ncd\nef\n1234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 7);
        assert_eq!(editor.get_cursor(), (4, 7));
        press(&mut editor, UP, 4);
        assert_eq!(editor.get_cursor(), (0, 7));
        press(&mut editor, DOWN, 4);
        assert_eq!(editor.get_cursor(), (4, 7));
    }

    #[test]
    fn moves_correctly_through_wrapped_visual_lines_without_getting_stuck() {
        let mut editor = editor_with("short\n123456789012345678901234567890");
        editor.render(15);
        assert_eq!(editor.get_cursor(), (1, 30));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor().0, 1);
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor().0, 1);
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor().0, 0);
    }

    #[test]
    fn handles_set_text_resetting_sticky_column() {
        let mut editor = editor_with("1234567890\n\n1234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 8);
        editor.handle_editor_input(UP);
        editor.set_text("abcdefghij\n\nabcdefghij");
        assert_eq!(editor.get_cursor(), (2, 10));
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 10));
    }

    #[test]
    fn sets_preferred_visual_col_when_pressing_right_at_end_of_prompt_last_line() {
        let mut editor = editor_with("111111111x1111111111\n\n333333333_");
        press(&mut editor, UP, 2);
        editor.handle_editor_input(LINE_END);
        assert_eq!(editor.get_cursor(), (0, 20));
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor(), (2, 10));
        editor.handle_editor_input(ARROW_RIGHT);
        assert_eq!(editor.get_cursor(), (2, 10));
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 10));
    }

    #[test]
    fn handles_editor_resizes_when_preferred_visual_col_is_on_the_same_line() {
        let mut editor = editor_with("12345678901234567890\n\n12345678901234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 15);
        press(&mut editor, UP, 2);
        assert_eq!(editor.get_cursor(), (0, 15));
        editor.render(12);
        press(&mut editor, DOWN, 2);
        assert_eq!(editor.get_cursor().1, 4);
    }

    #[test]
    fn handles_editor_resizes_when_preferred_visual_col_is_on_a_different_line() {
        let mut editor = editor_with("short\n12345678901234567890");
        editor.handle_editor_input(LINE_START);
        press(&mut editor, ARROW_RIGHT, 15);
        assert_eq!(editor.get_cursor(), (1, 15));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor(), (0, 5));
        editor.render(10);
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_cursor(), (1, 8));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor(), (0, 5));
        editor.render(80);
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_cursor(), (1, 15));
    }

    #[test]
    fn rewrapped_lines_target_fits_current_visual_column() {
        let mut editor = editor_with("abcdefghijklmnopqr\n123456789012345678");
        position_cursor(&mut editor, 0, 18);
        assert_eq!(editor.get_cursor(), (0, 18));
        editor.render(10);
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_cursor(), (1, 8));
        editor.render(80);
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor(), (0, 8));
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_cursor(), (1, 8));
    }

    #[test]
    fn rewrapped_lines_target_shorter_than_current_visual_column() {
        let mut editor = editor_with("abcdefghijklmnopqr\n123456789012345678\nab");
        position_cursor(&mut editor, 0, 18);
        assert_eq!(editor.get_cursor(), (0, 18));
        editor.render(10);
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_cursor(), (1, 8));
        editor.render(80);
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_cursor(), (2, 2));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor(), (1, 8));
    }
}

// senpi editor.test.ts "Word wrapping", "Grapheme-aware text wrapping" and "Scroll indicators".
mod wrapping {
    use super::*;
    use maho_tui::components::editor::word_wrap_line;
    use maho_tui::paste_markers::MarkerSegment;
    use maho_tui::tui::Component;

    fn rendered(text: &str, width: usize) -> Vec<String> {
        let (mut editor, _host) = create_editor(24);
        editor.set_text(text);
        editor.render(width)
    }

    fn chunk_texts(line: &str, width: usize) -> Vec<String> {
        word_wrap_line(line, width, None)
            .into_iter()
            .map(|chunk| chunk.text)
            .collect()
    }

    fn content(lines: &[String]) -> Vec<String> {
        lines[1..lines.len() - 1]
            .iter()
            .map(|line| strip_terminal_sequences(line).trim().to_owned())
            .collect()
    }

    fn assert_content_widths(lines: &[String], width: usize) {
        for (index, line) in lines[1..lines.len() - 1].iter().enumerate() {
            assert_eq!(
                visible_width(line),
                width,
                "line {} of {lines:?}",
                index + 1
            );
        }
    }

    /// Every chunk fits and the chunks' byte ranges rebuild the line.
    fn assert_fits_and_rebuilds(
        line: &str,
        width: usize,
        segments: Option<&[MarkerSegment]>,
    ) -> Vec<String> {
        let chunks = word_wrap_line(line, width, segments);
        for chunk in &chunks {
            assert!(
                visible_width(&chunk.text) <= width,
                "{:?} exceeds {width}",
                chunk.text
            );
        }
        let rebuilt: String = chunks
            .iter()
            .map(|chunk| &line[chunk.start_index..chunk.end_index])
            .collect();
        assert_eq!(rebuilt, line);
        chunks.into_iter().map(|chunk| chunk.text).collect()
    }

    fn segments(parts: &[&str]) -> Vec<MarkerSegment> {
        let mut index = 0;
        parts
            .iter()
            .map(|part| {
                let segment = MarkerSegment {
                    index,
                    segment: (*part).to_owned(),
                };
                index += part.len();
                segment
            })
            .collect()
    }

    const MARKER: &str = "[paste #1 +20 lines]";

    #[test]
    fn wraps_at_word_boundaries_instead_of_mid_word() {
        let lines = rendered(
            "Hello world this is a test of word wrapping functionality",
            40,
        );
        let content = content(&lines);
        assert!(!content[0].ends_with('-'));
        for line in content {
            let last = line.trim_end().chars().last();
            assert!(
                last.is_none_or(|ch| ch.is_alphanumeric() || ch == '_' || ".,!?;:".contains(ch)),
                "{line:?}"
            );
        }
    }

    #[test]
    fn does_not_start_lines_with_leading_whitespace_after_word_wrap() {
        let lines = rendered("Word1 Word2 Word3 Word4 Word5 Word6", 20);
        for line in &lines[1..lines.len() - 1] {
            let line = strip_terminal_sequences(line);
            let trimmed = line.trim_end();
            assert!(
                trimmed.trim_start().is_empty() || !trimmed.starts_with(char::is_whitespace),
                "{line:?}"
            );
        }
    }

    #[test]
    fn breaks_long_words_urls_at_character_level() {
        let lines = rendered(
            "Check https://example.com/very/long/path/that/exceeds/width here",
            30,
        );
        assert_content_widths(&lines, 30);
    }

    #[test]
    fn preserves_multiple_spaces_within_words_on_same_line() {
        let lines = rendered("Word1   Word2    Word3", 50);
        assert!(content(&lines)[0].contains("Word1   Word2"));
    }

    #[test]
    fn handles_empty_string() {
        assert_eq!(rendered("", 40).len(), 3);
    }

    #[test]
    fn handles_single_word_that_fits_exactly() {
        let lines = rendered("1234567890", 11);
        assert_eq!(lines.len(), 3);
        assert!(strip_terminal_sequences(&lines[1]).contains("1234567890"));
    }

    #[test]
    fn wraps_word_to_next_line_when_it_ends_exactly_at_terminal_width() {
        assert_eq!(
            chunk_texts("hello world test", 11),
            ["hello ", "world test"]
        );
    }

    #[test]
    fn keeps_whitespace_at_terminal_width_boundary_on_same_line() {
        assert_eq!(
            chunk_texts("hello world test", 12),
            ["hello world ", "test"]
        );
    }

    #[test]
    fn handles_unbreakable_word_filling_width_exactly_followed_by_space() {
        assert_eq!(
            chunk_texts("aaaaaaaaaaaa aaaa", 12),
            ["aaaaaaaaaaaa", " aaaa"]
        );
    }

    #[test]
    fn wraps_word_to_next_line_when_it_fits_width_but_not_remaining_space() {
        assert_eq!(
            chunk_texts("      aaaaaaaaaaaa", 12),
            ["      ", "aaaaaaaaaaaa"]
        );
    }

    #[test]
    fn keeps_word_with_multi_space_and_following_word_together_when_they_fit() {
        assert_eq!(
            chunk_texts("Lorem ipsum dolor sit amet,    consectetur", 30),
            ["Lorem ipsum dolor sit ", "amet,    consectetur"]
        );
    }

    #[test]
    fn keeps_word_with_multi_space_and_following_word_when_they_fill_width_exactly() {
        assert_eq!(
            chunk_texts("Lorem ipsum dolor sit amet,              consectetur", 30),
            ["Lorem ipsum dolor sit ", "amet,              consectetur"]
        );
    }

    #[test]
    fn splits_when_word_plus_multi_space_plus_word_exceeds_width() {
        assert_eq!(
            chunk_texts("Lorem ipsum dolor sit amet,               consectetur", 30),
            [
                "Lorem ipsum dolor sit ",
                "amet,               ",
                "consectetur"
            ]
        );
    }

    #[test]
    fn breaks_long_whitespace_at_line_boundary() {
        assert_eq!(
            chunk_texts(
                "Lorem ipsum dolor sit amet,                         consectetur",
                30
            ),
            [
                "Lorem ipsum dolor sit ",
                "amet,                         ",
                "consectetur"
            ]
        );
    }

    #[test]
    fn breaks_long_whitespace_at_line_boundary_2() {
        assert_eq!(
            chunk_texts(
                "Lorem ipsum dolor sit amet,                          consectetur",
                30
            ),
            [
                "Lorem ipsum dolor sit ",
                "amet,                         ",
                " consectetur"
            ]
        );
    }

    #[test]
    fn breaks_whitespace_spanning_full_lines() {
        assert_eq!(
            chunk_texts(
                "Lorem ipsum dolor sit amet,                                     consectetur",
                30
            ),
            [
                "Lorem ipsum dolor sit ",
                "amet,                         ",
                "            consectetur"
            ]
        );
    }

    #[test]
    fn force_breaks_when_wide_char_after_word_boundary_wrap_still_overflows() {
        let line = format!(" {}你", "a".repeat(186));
        assert_fits_and_rebuilds(&line, 187, None);
    }

    #[test]
    fn splits_oversized_atomic_segment_across_multiple_chunks() {
        let line = format!("A{MARKER}B");
        assert_fits_and_rebuilds(&line, 10, Some(&segments(&["A", MARKER, "B"])));
    }

    #[test]
    fn splits_oversized_atomic_segment_at_start_of_line() {
        let line = format!("{MARKER}B");
        let chunks = assert_fits_and_rebuilds(&line, 10, Some(&segments(&[MARKER, "B"])));
        assert!(chunks.last().is_some_and(|chunk| chunk.contains('B')));
    }

    #[test]
    fn splits_oversized_atomic_segment_at_end_of_line() {
        let line = format!("A{MARKER}");
        let chunks = assert_fits_and_rebuilds(&line, 10, Some(&segments(&["A", MARKER])));
        assert_eq!(chunks[0], "A");
    }

    #[test]
    fn splits_consecutive_oversized_atomic_segments() {
        let second = "[paste #2 +30 lines]";
        let line = format!("{MARKER}{second}");
        assert_fits_and_rebuilds(&line, 10, Some(&segments(&[MARKER, second])));
    }

    #[test]
    fn wraps_normally_after_oversized_atomic_segment() {
        let line = format!("{MARKER} hello world");
        let mut parts = vec![MARKER];
        let tail = " hello world";
        let singles: Vec<String> = tail.chars().map(String::from).collect();
        parts.extend(singles.iter().map(String::as_str));
        let chunks = assert_fits_and_rebuilds(&line, 10, Some(&segments(&parts)));
        assert_eq!(chunks.last().map(String::as_str), Some("world"));
    }

    #[test]
    fn wraps_lines_correctly_when_text_contains_wide_emojis() {
        assert_content_widths(&rendered("Hello ✅ World", 20), 20);
    }

    #[test]
    fn wraps_long_text_with_emojis_at_correct_positions() {
        assert_content_widths(&rendered("✅✅✅✅✅✅", 10), 10);
    }

    #[test]
    fn renders_isolated_thai_and_lao_am_clusters_without_width_drift() {
        for text in ["ำabc", "ຳabc"] {
            for line in rendered(text, 8) {
                assert_eq!(visible_width(&line), 8, "{text:?}: {line:?}");
            }
        }
    }

    #[test]
    fn wraps_cjk_characters_correctly_each_is_2_columns_wide() {
        let lines = rendered("日本語テスト", 11);
        assert_content_widths(&lines, 11);
        assert_eq!(content(&lines), ["日本語テス", "ト"]);
    }

    #[test]
    fn handles_mixed_ascii_and_wide_characters_in_wrapping() {
        let lines = rendered("Test ✅ OK 日本", 16);
        assert_eq!(lines.len(), 3);
        assert_eq!(visible_width(&lines[1]), 16);
    }

    #[test]
    fn renders_cursor_correctly_on_wide_characters() {
        let lines = rendered("A✅B", 20);
        assert!(lines[1].contains("\x1b[7m"));
        assert_eq!(visible_width(&lines[1]), 20);
    }

    #[test]
    fn does_not_exceed_terminal_width_with_emoji_at_wrap_boundary() {
        let lines = rendered("0123456789✅", 11);
        for line in &lines[1..lines.len() - 1] {
            assert!(visible_width(line) <= 11, "{line:?}");
        }
    }

    #[test]
    fn shows_cursor_at_end_of_line_before_wrap_wraps_on_next_char() {
        let width = 10;
        for padding_x in [0, 1] {
            let host = common::TestHost::new(24);
            let mut editor = Editor::new(
                host,
                common::plain_editor_theme(),
                EditorOptions {
                    padding_x: Some(padding_x),
                    ..Default::default()
                },
            );
            type_text(&mut editor, "aaaaaaaaa");
            let lines = editor.render(width + padding_x);
            assert_eq!(lines.len() - 2, 1, "padding {padding_x}");
            assert!(lines[1].ends_with("\x1b[7m \x1b[0m"), "{:?}", lines[1]);
            editor.handle_editor_input("a");
            let lines = editor.render(width + padding_x);
            assert_eq!(lines.len() - 2, 2, "padding {padding_x}");
        }
    }

    #[test]
    fn centers_scroll_indicators_on_wide_borders() {
        let width = 40;
        let text = (0..20)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (mut editor, _host) = create_editor(24);
        editor.set_text(&text);
        editor.render(width);
        for _ in 0..10 {
            editor.handle_editor_input("\x1b[A");
        }
        let lines = editor.render(width);
        let rule = "─".repeat(15);
        assert_eq!(
            strip_terminal_sequences(&lines[0]),
            format!("{rule} ↑ 9 more {rule}")
        );
        assert_eq!(
            strip_terminal_sequences(lines.last().expect("bottom border")),
            format!("{rule} ↓ 4 more {rule}")
        );
    }

    #[test]
    fn keeps_truncated_scroll_indicators_within_width_and_preserves_their_color_issue_6962() {
        let width = 10;
        let color = |text: &str| format!("\x1b[35m{text}\x1b[39m");
        let mut theme = common::plain_editor_theme();
        theme.border_color = Rc::new(color);
        let (mut editor, _host) = create_editor_with_theme(theme, 24);
        let text = (0..20)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        editor.set_text(&text);
        editor.render(width);
        for _ in 0..10 {
            editor.handle_editor_input("\x1b[A");
        }
        let lines = editor.render(width);
        let top = &lines[0];
        let bottom = lines.last().expect("bottom border");
        assert!(
            strip_terminal_sequences(top).starts_with("─── ↑"),
            "{top:?}"
        );
        assert!(
            strip_terminal_sequences(bottom).starts_with("─── ↓"),
            "{bottom:?}"
        );
        assert_eq!(*top, color(&strip_terminal_sequences(top)));
        assert_eq!(*bottom, color(&strip_terminal_sequences(bottom)));
        for line in &lines {
            assert_eq!(visible_width(line), width, "{line:?}");
        }
    }
}

// senpi editor.test.ts "Prompt history navigation", "Warp-on-WSL Shift+Enter normalization",
// "Kitty CSI-u handling" and "Unicode text editing behavior" cases not ported above. Cursor columns
// are byte offsets here (UTF-16 units upstream); `col` converts upstream's character columns.
mod history_and_input {
    use super::*;

    const UP: &str = "\x1b[A";
    const DOWN: &str = "\x1b[B";
    const CTRL_LEFT: &str = "\x1b[1;5D";
    const CTRL_RIGHT: &str = "\x1b[1;5C";
    const EXPLICIT_NEWLINE: &str = "\x1b[13;2u";

    fn editor() -> Editor {
        create_editor(24).0
    }

    /// The byte column of character column `chars` in `line`.
    fn col(line: &str, chars: usize) -> usize {
        line.char_indices()
            .nth(chars)
            .map_or(line.len(), |(index, _)| index)
    }

    fn submitted(editor: &mut Editor) -> Rc<RefCell<Option<String>>> {
        let submitted = Rc::new(RefCell::new(None));
        let sink = submitted.clone();
        editor.on_submit = Some(Box::new(move |text: &str| {
            *sink.borrow_mut() = Some(text.to_owned());
        }));
        submitted
    }

    #[test]
    fn jumps_to_start_before_entering_history_from_a_non_empty_draft() {
        let mut editor = editor();
        editor.add_to_history("prompt");
        editor.set_text("draft");
        editor.handle_editor_input(ARROW_LEFT);
        editor.handle_editor_input(ARROW_LEFT);
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_text(), "draft");
        assert_eq!(editor.get_cursor(), (0, 0));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_text(), "prompt");
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_text(), "draft");
        assert_eq!(editor.get_cursor(), (0, 0));
    }

    #[test]
    fn exits_history_mode_on_set_text_and_restarts_from_the_most_recent() {
        let mut editor = editor();
        editor.add_to_history("first");
        editor.add_to_history("second");
        editor.handle_editor_input(UP);
        editor.set_text("");
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_text(), "second");
    }

    #[test]
    fn uses_cursor_movement_instead_of_history_when_editor_has_content() {
        let mut editor = editor();
        editor.add_to_history("history item");
        editor.set_text("line1\nline2");
        editor.handle_editor_input(UP);
        editor.handle_editor_input("X");
        assert_eq!(editor.get_text(), "line1X\nline2");
    }

    #[test]
    fn places_cursor_at_start_after_browsing_history_upward() {
        let mut editor = editor();
        editor.add_to_history("older entry");
        editor.add_to_history("line1\nline2\nline3");
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
        assert_eq!(editor.get_cursor(), (0, 0));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_text(), "older entry");
        assert_eq!(editor.get_cursor(), (0, 0));
    }

    #[test]
    fn places_cursor_at_end_after_browsing_history_downward() {
        let mut editor = editor();
        editor.add_to_history("older entry");
        editor.add_to_history("line1\nline2\nline3");
        editor.add_to_history("newer entry");
        for _ in 0..3 {
            editor.handle_editor_input(UP);
        }
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
        assert_eq!(editor.get_cursor(), (2, 5));
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_text(), "newer entry");
    }

    #[test]
    fn allows_opposite_direction_cursor_movement_within_multi_line_history_entry() {
        let mut editor = editor();
        editor.add_to_history("line1\nline2\nline3");
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_cursor(), (0, 0));
        editor.handle_editor_input(DOWN);
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
        assert_eq!(editor.get_cursor(), (1, 0));
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
        assert_eq!(editor.get_cursor(), (0, 0));
    }

    #[test]
    fn keeps_cr_submission_and_non_warp_lf_behavior_unchanged() {
        let mut warp = warp_editor();
        let warp_submitted = submitted(&mut warp);
        warp.set_text("warp");
        warp.handle_editor_input("\r");
        assert_eq!(warp_submitted.borrow().as_deref(), Some("warp"));

        let mut regular = Editor::new(
            TestHost::new(24),
            common::plain_editor_theme(),
            EditorOptions {
                terminal_environment: Some(maho_tui::process_env::env_from(&[])),
                terminal_platform: Some("linux".to_string()),
                ..Default::default()
            },
        );
        let regular_submitted = submitted(&mut regular);
        regular.set_text("regular");
        regular.handle_editor_input("\n");
        assert_eq!(regular_submitted.borrow().as_deref(), Some("regular"));
        assert_eq!(regular.get_text(), "");
    }

    #[test]
    fn inserts_backslash_immediately_no_buffering() {
        let mut editor = editor();
        editor.handle_editor_input("\\");
        assert_eq!(editor.get_text(), "\\");
    }

    #[test]
    fn inserts_a_newline_for_tmux_csi_u_shift_enter_without_submitting() {
        let mut editor = editor();
        let submitted = submitted(&mut editor);
        editor.set_text("hello");
        editor.handle_editor_input("\x1b[13;2u");
        assert_eq!(editor.get_text(), "hello\n");
        assert!(submitted.borrow().is_none());
    }

    #[test]
    fn ignores_printable_csi_u_sequences_with_unsupported_modifiers() {
        let mut editor = editor();
        editor.handle_editor_input("\x1b[99;9u");
        assert_eq!(editor.get_text(), "");
    }

    #[test]
    fn inserts_shifted_csi_u_letters_as_text() {
        let mut editor = editor();
        editor.handle_editor_input("\x1b[69;2u");
        assert_eq!(editor.get_text(), "E");
    }

    #[test]
    fn inserts_shifted_xterm_modify_other_keys_letters_as_text() {
        let mut editor = editor();
        editor.handle_editor_input("\x1b[27;2;69~");
        assert_eq!(editor.get_text(), "E");
    }

    #[test]
    fn inserts_mixed_ascii_umlauts_and_emojis_as_literal_text() {
        let mut editor = editor();
        for key in ["H", "e", "l", "l", "o", " ", "ä", "ö", "ü", " ", "😀"] {
            editor.handle_editor_input(key);
        }
        assert_eq!(editor.get_text(), "Hello äöü 😀");
    }

    #[test]
    fn deletes_single_code_unit_unicode_characters_umlauts_with_backspace() {
        let mut editor = editor();
        type_text(&mut editor, "äöü");
        editor.handle_editor_input(BACKSPACE);
        assert_eq!(editor.get_text(), "äö");
    }

    #[test]
    fn preserves_umlauts_across_line_breaks() {
        let mut editor = editor();
        type_text(&mut editor, "äöü");
        editor.handle_editor_input(EXPLICIT_NEWLINE);
        type_text(&mut editor, "ÄÖÜ");
        assert_eq!(editor.get_text(), "äöü\nÄÖÜ");
    }

    #[test]
    fn deletes_words_correctly_with_ctrl_w_and_alt_backspace() {
        let mut editor = editor();
        let cases = [
            ("foo bar baz", "foo bar "),
            ("foo bar   ", "foo "),
            ("foo bar...", "foo bar"),
            ("foo.bar", "foo."),
            ("foo:bar", "foo:"),
            ("line one\nline two", "line one\nline "),
            ("line one\n", "line one"),
        ];
        for (text, expected) in cases {
            editor.set_text(text);
            editor.handle_editor_input("\x17");
            assert_eq!(editor.get_text(), expected, "{text:?}");
        }
        editor.set_text("foo 😀😀 bar");
        editor.handle_editor_input("\x17");
        assert_eq!(editor.get_text(), "foo 😀😀 ");
        editor.handle_editor_input("\x17");
        assert_eq!(editor.get_text(), "foo ");
        editor.set_text("foo bar");
        editor.handle_editor_input("\x1b\x7f");
        assert_eq!(editor.get_text(), "foo ");
    }

    #[test]
    fn navigates_words_correctly_with_ctrl_left_right() {
        let mut editor = editor();
        editor.set_text("foo bar... baz");
        for (key, expected) in [
            (CTRL_LEFT, 11),
            (CTRL_LEFT, 7),
            (CTRL_LEFT, 4),
            (CTRL_RIGHT, 7),
            (CTRL_RIGHT, 10),
            (CTRL_RIGHT, 14),
        ] {
            editor.handle_editor_input(key);
            assert_eq!(editor.get_cursor(), (0, expected));
        }
        editor.set_text("   foo bar");
        editor.handle_editor_input(LINE_START);
        editor.handle_editor_input(CTRL_RIGHT);
        assert_eq!(editor.get_cursor(), (0, 6));
        editor.set_text("foo.bar baz");
        for (key, expected) in [(CTRL_LEFT, 8), (CTRL_LEFT, 4), (CTRL_LEFT, 3)] {
            editor.handle_editor_input(key);
            assert_eq!(editor.get_cursor(), (0, expected));
        }
        editor.handle_editor_input(LINE_START);
        for (key, expected) in [(CTRL_RIGHT, 3), (CTRL_RIGHT, 4), (CTRL_RIGHT, 7)] {
            editor.handle_editor_input(key);
            assert_eq!(editor.get_cursor(), (0, expected));
        }
    }

    #[test]
    fn stops_at_fullwidth_chinese_punctuation_issue_4972() {
        let text = "你好，世界";
        let mut editor = editor();
        editor.set_text(text);
        for (key, chars) in [
            (CTRL_LEFT, 3),
            (CTRL_LEFT, 2),
            (CTRL_LEFT, 0),
            (CTRL_RIGHT, 2),
            (CTRL_RIGHT, 3),
            (CTRL_RIGHT, 5),
        ] {
            editor.handle_editor_input(key);
            assert_eq!(editor.get_cursor(), (0, col(text, chars)));
        }
    }

    #[test]
    fn handles_mixed_cjk_and_ascii_word_movement() {
        let text = "hello你好，world世界";
        let mut editor = editor();
        editor.set_text(text);
        for (key, chars) in [
            (CTRL_LEFT, 13),
            (CTRL_LEFT, 8),
            (CTRL_LEFT, 7),
            (CTRL_LEFT, 5),
            (CTRL_LEFT, 0),
            (CTRL_RIGHT, 5),
            (CTRL_RIGHT, 7),
            (CTRL_RIGHT, 8),
            (CTRL_RIGHT, 13),
            (CTRL_RIGHT, 15),
        ] {
            editor.handle_editor_input(key);
            assert_eq!(editor.get_cursor(), (0, col(text, chars)));
        }
    }
}

// senpi editor.test.ts "Kill ring", "Undo" and "Autocomplete" cases not ported above. senpi's
// debounce timers are driven here by `poll_autocomplete_at(Instant)` (no real waiting).
mod kill_ring_undo_autocomplete {
    use super::*;

    const UP: &str = "\x1b[A";
    const EXPLICIT_NEWLINE: &str = "\x1b[13;2u";
    const CTRL_W: &str = "\x17";
    const CTRL_U: &str = "\x15";
    const CTRL_K: &str = "\x0b";
    const CTRL_Y: &str = "\x19";
    const ALT_Y: &str = "\x1by";
    const TAB: &str = "\t";

    fn editor() -> Editor {
        create_editor(24).0
    }

    fn now_plus(ms: u64) -> std::time::Instant {
        std::time::Instant::now() + std::time::Duration::from_millis(ms)
    }

    /// A synchronous provider: `suggest(text before cursor, force)`; senpi's prefix-replacing
    /// `applyCompletion`; `calls` counts lookups.
    struct ScriptedProvider {
        suggest: Box<dyn Fn(&str, bool) -> Option<AutocompleteSuggestions>>,
        triggers: Vec<String>,
        calls: Rc<RefCell<usize>>,
    }
    impl AutocompleteProvider for ScriptedProvider {
        fn trigger_characters(&self) -> Vec<String> {
            self.triggers.clone()
        }
        fn get_suggestions(
            &mut self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            force: bool,
        ) -> Option<AutocompleteSuggestions> {
            *self.calls.borrow_mut() += 1;
            let line = lines.get(cursor_line).map_or("", String::as_str);
            (self.suggest)(&line[..cursor_col.min(line.len())], force)
        }
        fn apply_completion(
            &self,
            lines: &[String],
            cursor_line: usize,
            cursor_col: usize,
            item: &AutocompleteItem,
            prefix: &str,
        ) -> ApplyCompletionResult {
            let line = lines.get(cursor_line).cloned().unwrap_or_default();
            let start = cursor_col - prefix.len();
            let mut lines = lines.to_vec();
            lines[cursor_line] = format!("{}{}{}", &line[..start], item.value, &line[cursor_col..]);
            ApplyCompletionResult {
                lines,
                cursor_line,
                cursor_col: start + item.value.len(),
            }
        }
    }

    fn items(values: &[&str], prefix: &str) -> Option<AutocompleteSuggestions> {
        Some(AutocompleteSuggestions {
            items: values
                .iter()
                .map(|value| AutocompleteItem {
                    value: (*value).to_owned(),
                    label: (*value).to_owned(),
                    description: None,
                })
                .collect(),
            prefix: prefix.to_owned(),
        })
    }

    fn provide(
        editor: &mut Editor,
        triggers: &[&str],
        suggest: impl Fn(&str, bool) -> Option<AutocompleteSuggestions> + 'static,
    ) -> Rc<RefCell<usize>> {
        let calls = Rc::new(RefCell::new(0));
        editor.set_autocomplete_provider(Rc::new(RefCell::new(ScriptedProvider {
            suggest: Box::new(suggest),
            triggers: triggers.iter().map(|value| (*value).to_owned()).collect(),
            calls: calls.clone(),
        })));
        calls
    }

    #[test]
    fn ctrl_u_accumulates_multiline_deletes_including_newlines() {
        let mut editor = editor();
        editor.set_text("line1\nline2\nline3");
        for expected in ["line1\nline2\n", "line1\nline2", "line1\n", "line1", ""] {
            editor.handle_editor_input(CTRL_U);
            assert_eq!(editor.get_text(), expected);
        }
        editor.handle_editor_input(CTRL_Y);
        assert_eq!(editor.get_text(), "line1\nline2\nline3");
    }

    #[test]
    fn consecutive_deletions_across_lines_coalesce_into_one_entry() {
        let mut editor = editor();
        editor.set_text("1\n2\n3");
        for expected in ["1\n2\n", "1\n2", "1\n", "1", ""] {
            editor.handle_editor_input(CTRL_W);
            assert_eq!(editor.get_text(), expected);
        }
        editor.handle_editor_input(CTRL_Y);
        assert_eq!(editor.get_text(), "1\n2\n3");
    }

    #[test]
    fn ctrl_k_at_line_end_deletes_newline_and_coalesces() {
        let mut editor = editor();
        type_text(&mut editor, "ab");
        editor.handle_editor_input(EXPLICIT_NEWLINE);
        type_text(&mut editor, "cd");
        editor.handle_editor_input(UP);
        editor.handle_editor_input(LINE_END);
        editor.handle_editor_input(CTRL_K);
        assert_eq!(editor.get_text(), "abcd");
        editor.handle_editor_input(CTRL_K);
        assert_eq!(editor.get_text(), "ab");
        editor.handle_editor_input(CTRL_Y);
        assert_eq!(editor.get_text(), "ab\ncd");
    }

    #[test]
    fn multiline_yank_and_yank_pop_in_middle_of_text() {
        let mut editor = editor();
        editor.set_text("SINGLE");
        editor.handle_editor_input(CTRL_W);
        editor.set_text("A\nB");
        for _ in 0..3 {
            editor.handle_editor_input(CTRL_U);
        }
        editor.set_text("hello world");
        editor.handle_editor_input(LINE_START);
        for _ in 0..6 {
            editor.handle_editor_input(ARROW_RIGHT);
        }
        editor.handle_editor_input(CTRL_Y);
        assert_eq!(editor.get_text(), "hello A\nBworld");
        editor.handle_editor_input(ALT_Y);
        assert_eq!(editor.get_text(), "hello SINGLEworld");
    }

    #[test]
    fn alt_d_at_end_of_line_deletes_newline() {
        let mut editor = editor();
        editor.set_text("line1\nline2");
        editor.handle_editor_input(UP);
        editor.handle_editor_input(LINE_END);
        editor.handle_editor_input("\x1bd");
        assert_eq!(editor.get_text(), "line1line2");
        editor.handle_editor_input(CTRL_Y);
        assert_eq!(editor.get_text(), "line1\nline2");
    }

    #[test]
    fn undoes_newlines_and_signals_next_word_to_capture_state() {
        let mut editor = editor();
        type_text(&mut editor, "hello");
        editor.handle_editor_input(EXPLICIT_NEWLINE);
        type_text(&mut editor, "world");
        assert_eq!(editor.get_text(), "hello\nworld");
        for expected in ["hello\n", "hello", ""] {
            editor.handle_editor_input(UNDO);
            assert_eq!(editor.get_text(), expected);
        }
    }

    #[test]
    fn does_not_trigger_autocomplete_during_single_line_paste() {
        let mut editor = editor();
        let calls = provide(&mut editor, &[], |_, _| None);
        editor.handle_editor_input("\x1b[200~look at @node_modules/react/index.js please\x1b[201~");
        editor.poll_autocomplete_at(now_plus(1_000));
        assert_eq!(
            editor.get_text(),
            "look at @node_modules/react/index.js please"
        );
        assert_eq!(*calls.borrow(), 0);
        assert!(!editor.is_showing_autocomplete());
    }

    #[test]
    fn undoes_set_text_to_empty_string() {
        let mut editor = editor();
        type_text(&mut editor, "hello world");
        editor.set_text("");
        assert_eq!(editor.get_text(), "");
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "hello world");
    }

    #[test]
    fn exits_history_browsing_mode_on_undo() {
        let mut editor = editor();
        editor.add_to_history("hello");
        type_text(&mut editor, "world");
        editor.handle_editor_input(CTRL_W);
        assert_eq!(editor.get_text(), "");
        editor.handle_editor_input(UP);
        assert_eq!(editor.get_text(), "hello");
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "");
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "world");
    }

    #[test]
    fn undo_restores_to_pre_history_state_even_after_multiple_history_navigations() {
        let mut editor = editor();
        for entry in ["first", "second", "third"] {
            editor.add_to_history(entry);
        }
        type_text(&mut editor, "current");
        editor.handle_editor_input(CTRL_W);
        for expected in ["third", "second", "first"] {
            editor.handle_editor_input(UP);
            assert_eq!(editor.get_text(), expected);
        }
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "");
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "current");
    }

    #[test]
    fn no_op_delete_operations_do_not_push_undo_snapshots() {
        let mut editor = editor();
        type_text(&mut editor, "hello");
        editor.handle_editor_input(CTRL_W);
        assert_eq!(editor.get_text(), "");
        editor.handle_editor_input(CTRL_W);
        editor.handle_editor_input(CTRL_W);
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "hello");
    }

    #[test]
    fn undoes_autocomplete() {
        let mut editor = editor();
        provide(&mut editor, &[], |prefix, _| {
            (prefix == "di").then(|| items(&["dist/"], "di")).flatten()
        });
        type_text(&mut editor, "di");
        editor.handle_editor_input(TAB);
        editor.poll_autocomplete_at(now_plus(1_000));
        assert_eq!(editor.get_text(), "dist/");
        assert!(!editor.is_showing_autocomplete());
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "di");
    }

    #[test]
    fn auto_applies_single_force_file_suggestion_without_showing_menu() {
        let mut editor = editor();
        provide(&mut editor, &[], |prefix, force| {
            (force && prefix == "Work")
                .then(|| items(&["Workspace/"], "Work"))
                .flatten()
        });
        type_text(&mut editor, "Work");
        editor.handle_editor_input(TAB);
        editor.poll_autocomplete_at(now_plus(1_000));
        assert_eq!(editor.get_text(), "Workspace/");
        assert!(!editor.is_showing_autocomplete());
        editor.handle_editor_input(UNDO);
        assert_eq!(editor.get_text(), "Work");
    }

    #[test]
    fn shows_menu_when_force_file_has_multiple_suggestions() {
        let mut editor = editor();
        provide(&mut editor, &[], |prefix, force| {
            (force && prefix == "src")
                .then(|| items(&["src/", "src.txt"], "src"))
                .flatten()
        });
        type_text(&mut editor, "src");
        editor.handle_editor_input(TAB);
        editor.poll_autocomplete_at(now_plus(1_000));
        assert_eq!(editor.get_text(), "src");
        assert!(editor.is_showing_autocomplete());
        editor.handle_editor_input(TAB);
        assert_eq!(editor.get_text(), "src/");
        assert!(!editor.is_showing_autocomplete());
    }

    #[test]
    fn keeps_suggestions_open_when_typing_in_force_mode_tab_triggered() {
        let mut editor = editor();
        provide(&mut editor, &[], |prefix, force| {
            if !(force || prefix.contains('/') || prefix.starts_with('.')) {
                return None;
            }
            let all = ["readme.md", "package.json", "src/", "dist/"];
            let matched: Vec<&str> = all
                .into_iter()
                .filter(|value| value.to_lowercase().starts_with(&prefix.to_lowercase()))
                .collect();
            (!matched.is_empty())
                .then(|| items(&matched, prefix))
                .flatten()
        });
        editor.handle_editor_input(TAB);
        editor.poll_autocomplete_at(now_plus(1_000));
        assert!(editor.is_showing_autocomplete());
        for (key, text) in [("r", "r"), ("e", "re")] {
            editor.handle_editor_input(key);
            editor.poll_autocomplete_at(now_plus(1_000));
            assert_eq!(editor.get_text(), text);
            assert!(editor.is_showing_autocomplete());
        }
        editor.handle_editor_input(TAB);
        assert_eq!(editor.get_text(), "readme.md");
        assert!(!editor.is_showing_autocomplete());
    }

    /// Typing `keys` makes no lookup until the host polls past the debounce, then exactly one.
    fn assert_debounced(triggers: &[&str], keys: &str, value: &'static str) {
        let mut editor = editor();
        let calls = provide(&mut editor, triggers, move |prefix, _| {
            items(&[value], prefix)
        });
        type_text(&mut editor, keys);
        assert_eq!(*calls.borrow(), 0, "{keys:?} waits for the debounce");
        assert!(!editor.is_showing_autocomplete());
        editor.poll_autocomplete_at(now_plus(1_000));
        editor.poll_autocomplete_at(now_plus(1_000));
        assert_eq!(*calls.borrow(), 1, "{keys:?}");
        assert!(editor.is_showing_autocomplete());
    }

    #[test]
    fn debounces_at_autocomplete_while_typing() {
        assert_debounced(&[], "@mai", "@main.ts");
    }

    #[test]
    fn debounces_hash_autocomplete_while_typing() {
        assert_debounced(&[], "#298", "#2983");
    }

    #[test]
    fn debounces_custom_trigger_characters_autocomplete_while_typing() {
        assert_debounced(&["%"], "%sk", "%skill-name");
    }

    #[test]
    fn resets_custom_trigger_characters_when_provider_changes() {
        let mut editor = editor();
        provide(&mut editor, &["%"], |_, _| items(&["%skill-name"], "%"));
        let calls = provide(&mut editor, &[], |_, _| items(&["%skill-name"], "%"));
        type_text(&mut editor, "%s");
        editor.poll_autocomplete_at(now_plus(1_000));
        editor.poll_autocomplete_at(now_plus(1_000));
        assert_eq!(*calls.borrow(), 0);
        assert!(!editor.is_showing_autocomplete());
    }

    #[test]
    fn hides_autocomplete_when_backspacing_slash_command_to_empty() {
        let mut editor = editor();
        provide(&mut editor, &[], |prefix, _| {
            let query = prefix.strip_prefix('/')?;
            let matched: Vec<&str> = ["/model", "/help"]
                .into_iter()
                .filter(|value| value.starts_with(query))
                .collect();
            (!matched.is_empty())
                .then(|| items(&matched, prefix))
                .flatten()
        });
        editor.handle_editor_input("/");
        editor.poll_autocomplete_at(now_plus(1_000));
        assert_eq!(editor.get_text(), "/");
        assert!(editor.is_showing_autocomplete());
        editor.handle_editor_input(BACKSPACE);
        editor.poll_autocomplete_at(now_plus(1_000));
        assert_eq!(editor.get_text(), "");
        assert!(!editor.is_showing_autocomplete());
    }
}
