//! Port of the editor behaviors of senpi `packages/tui/test/editor.test.ts`,
//! `editor-image-marker.test.ts`, `editor-mention-highlight.test.ts`,
//! `editor-history-keybindings.test.ts` and `editor-render-row.test.ts`.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{create_editor, create_editor_with_theme, large_paste, paste, TestHost};
use maho_tui::autocomplete::{
    ApplyCompletionResult, AutocompleteItem, AutocompleteProvider, AutocompleteSuggestions,
    MentionRange,
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
    assert_eq!(editor.get_text(), "[paste #1 +12 lines][paste #2 +12 lines]");

    editor.handle_editor_input(LINE_START);
    editor.handle_editor_input(FORWARD_DELETE);
    assert_eq!(editor.get_text(), "[paste #1 +12 lines]");
    assert_eq!(editor.get_expanded_text(), body_two);

    editor.handle_editor_input(UNDO);
    assert_eq!(editor.get_text(), "[paste #1 +12 lines][paste #2 +12 lines]");
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
        lines.iter().any(|line| line.contains(&styled("$debugging"))),
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
