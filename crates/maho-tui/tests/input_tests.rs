//! Port of senpi `packages/tui/test/input.test.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use maho_tui::components::input::{Input, InputOptions};
use maho_tui::tui::Component;
use maho_tui::utils::{strip_terminal_sequences, visible_width};

fn focused_input(options: InputOptions) -> Input {
    let mut input = Input::new(options);
    input.focusable_set(true);
    input
}

#[test]
fn submits_value_including_backslash_on_enter() {
    let mut input = Input::new(InputOptions::default());
    let submitted = Rc::new(RefCell::new(None::<String>));
    let sink = Rc::clone(&submitted);
    input.on_submit = Some(Box::new(move |value: &str| {
        *sink.borrow_mut() = Some(value.to_string());
    }));
    for key in ["h", "e", "l", "l", "o", "\\", "\r"] {
        input.handle_input(key);
    }
    assert_eq!(submitted.borrow().as_deref(), Some("hello\\"));
}

#[test]
fn inserts_backslash_as_regular_character() {
    let mut input = Input::new(InputOptions::default());
    input.handle_input("\\");
    input.handle_input("x");
    assert_eq!(input.get_value(), "\\x");
}

#[test]
fn supports_a_custom_prompt_and_styled_placeholder() {
    let mut input = focused_input(InputOptions {
        prompt: Some(String::new()),
        placeholder: Some("Find transcript".to_string()),
        placeholder_style: Some(Rc::new(|text: &str| format!("\x1b[2m{text}\x1b[22m"))),
    });
    let empty = input.render(20).remove(0);
    assert!(empty.contains("\x1b[2m"));
    assert_eq!(
        strip_terminal_sequences(&empty).trim_end(),
        "Find transcript"
    );

    input.handle_input("n");
    let populated = input.render(20).remove(0);
    assert_eq!(strip_terminal_sequences(&populated).trim_end(), "n");
}

#[test]
fn does_not_overflow_with_wide_cjk_and_fullwidth_text() {
    type CursorPosition = (&'static str, fn(&mut Input));

    let width = 93;
    let cases = [
        "中文汉字测试用于验证文本超出终端宽度时界面不会崩溃这是复现用的测试用例",
        "これはテスト文章です。日本語のテキストが正しく表示されるかどうかを確認するためのサンプルテキストです。あいうえお",
        "这是一段测试文本，用于验证中文字符在终端中的显示宽度是否被正确计算，如果不正确就会导致用户界面崩溃的问题",
        "ＡＢＣＤＥＦＧＨＩＪＫＬＭＮＯＰＱＲＳＴＵＶＷＸＹＺ０１２３４５６７８９ａｂｃｄｅｆｇｈｉｊｋｌｍ",
    ];
    let cursor_positions: [CursorPosition; 3] = [
        ("start", |_| {}),
        ("middle", |input| {
            for _ in 0..10 {
                input.handle_input("\x1b[C");
            }
        }),
        ("end", |input| input.handle_input("\x05")),
    ];
    for text in cases {
        for (label, move_cursor) in cursor_positions {
            let mut input = Input::new(InputOptions::default());
            input.set_value(text);
            input.focusable_set(true);
            move_cursor(&mut input);
            let line = input.render(width).remove(0);
            assert!(
                visible_width(&line) <= width,
                "rendered line overflowed for {text} at {label}"
            );
        }
    }
}

#[test]
fn keeps_the_cursor_visible_when_horizontally_scrolling_wide_text() {
    let width = 20;
    let mut input = Input::new(InputOptions::default());
    input.set_value("中文汉字测试宽度文字");
    input.focusable_set(true);
    input.handle_input("\x01");
    for _ in 0..5 {
        input.handle_input("\x1b[C");
    }
    let line = input.render(width).remove(0);
    assert!(visible_width(&line) <= width);
}

#[test]
fn ctrl_w_saves_deleted_text_to_kill_ring_and_ctrl_y_yanks_it() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("foo bar baz");
    // Move cursor to end
    input.handle_input("\x05"); // Ctrl+E

    input.handle_input("\x17"); // Ctrl+W - deletes "baz"
    assert_eq!(input.get_value(), "foo bar ");

    // Move to beginning and yank
    input.handle_input("\x01"); // Ctrl+A
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "bazfoo bar ");
}

#[test]
fn ctrl_w_preserves_ascii_punctuation_boundaries() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("foo.bar");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "bar"
    assert_eq!(input.get_value(), "foo.");

    input.set_value("foo:bar");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "bar"
    assert_eq!(input.get_value(), "foo:");
}

#[test]
fn ctrl_w_handles_unicode_word_boundaries() {
    let mut input = Input::new(InputOptions::default());

    // "你好世界。你好，世界" segments as: 你好|世界|。|你好|，|世界
    input.set_value("你好世界。你好，世界");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "世界"
    assert_eq!(input.get_value(), "你好世界。你好，");
    input.handle_input("\x17"); // Ctrl+W - deletes "，"
    assert_eq!(input.get_value(), "你好世界。你好");
    input.handle_input("\x17"); // Ctrl+W - deletes "你好"
    assert_eq!(input.get_value(), "你好世界。");
    input.handle_input("\x17"); // Ctrl+W - deletes "。"
    assert_eq!(input.get_value(), "你好世界");
    input.handle_input("\x17"); // Ctrl+W - deletes "世界"
    assert_eq!(input.get_value(), "你好");
    input.handle_input("\x17"); // Ctrl+W - deletes "你好"
    assert_eq!(input.get_value(), "");
}

#[test]
fn ctrl_u_saves_deleted_text_to_kill_ring() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("hello world");
    // Move cursor to after "hello "
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x15"); // Ctrl+U - deletes "hello "
    assert_eq!(input.get_value(), "world");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn ctrl_k_saves_deleted_text_to_kill_ring() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("hello world");
    input.handle_input("\x01"); // Ctrl+A
    input.handle_input("\x0b"); // Ctrl+K - deletes "hello world"

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn ctrl_y_does_nothing_when_kill_ring_is_empty() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("test");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "test");
}

#[test]
fn alt_y_cycles_through_kill_ring_after_ctrl_y() {
    let mut input = Input::new(InputOptions::default());

    // Create kill ring with multiple entries
    input.set_value("first");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "first"
    input.set_value("second");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "second"
    input.set_value("third");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "third"

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19"); // Ctrl+Y - yanks "third"
    assert_eq!(input.get_value(), "third");

    input.handle_input("\x1by"); // Alt+Y - cycles to "second"
    assert_eq!(input.get_value(), "second");

    input.handle_input("\x1by"); // Alt+Y - cycles to "first"
    assert_eq!(input.get_value(), "first");

    input.handle_input("\x1by"); // Alt+Y - cycles back to "third"
    assert_eq!(input.get_value(), "third");
}

#[test]
fn alt_y_does_nothing_if_not_preceded_by_yank() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("test");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "test"
    input.set_value("other");
    input.handle_input("\x05"); // Ctrl+E

    // Type something to break the yank chain
    input.handle_input("x");
    assert_eq!(input.get_value(), "otherx");

    input.handle_input("\x1by"); // Alt+Y - should do nothing
    assert_eq!(input.get_value(), "otherx");
}

#[test]
fn alt_y_does_nothing_if_kill_ring_has_one_entry() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("only");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "only"

    input.handle_input("\x19"); // Ctrl+Y - yanks "only"
    assert_eq!(input.get_value(), "only");

    input.handle_input("\x1by"); // Alt+Y - should do nothing
    assert_eq!(input.get_value(), "only");
}

#[test]
fn consecutive_ctrl_w_accumulates_into_one_kill_ring_entry() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("one two three");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "three"
    input.handle_input("\x17"); // Ctrl+W - deletes "two "
    input.handle_input("\x17"); // Ctrl+W - deletes "one "

    assert_eq!(input.get_value(), "");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "one two three");
}

#[test]
fn non_delete_actions_break_kill_accumulation() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("foo bar baz");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "baz"
    assert_eq!(input.get_value(), "foo bar ");

    input.handle_input("x"); // Typing breaks accumulation
    assert_eq!(input.get_value(), "foo bar x");

    input.handle_input("\x17"); // Ctrl+W - deletes "x" (separate entry)
    assert_eq!(input.get_value(), "foo bar ");

    input.handle_input("\x19"); // Ctrl+Y - most recent is "x"
    assert_eq!(input.get_value(), "foo bar x");

    input.handle_input("\x1by"); // Alt+Y - cycle to "baz"
    assert_eq!(input.get_value(), "foo bar baz");
}

#[test]
fn non_yank_actions_break_alt_y_chain() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("first");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W
    input.set_value("second");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W
    input.set_value("");

    input.handle_input("\x19"); // Ctrl+Y - yanks "second"
    assert_eq!(input.get_value(), "second");

    input.handle_input("x"); // Breaks yank chain
    assert_eq!(input.get_value(), "secondx");

    input.handle_input("\x1by"); // Alt+Y - should do nothing
    assert_eq!(input.get_value(), "secondx");
}

#[test]
fn kill_ring_rotation_persists_after_cycling() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("first");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // deletes "first"
    input.set_value("second");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // deletes "second"
    input.set_value("third");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // deletes "third"
    input.set_value("");

    input.handle_input("\x19"); // Ctrl+Y - yanks "third"
    input.handle_input("\x1by"); // Alt+Y - cycles to "second"
    assert_eq!(input.get_value(), "second");

    // Break chain and start fresh
    input.handle_input("x");
    input.set_value("");

    // New yank should get "second" (now at end after rotation)
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "second");
}

#[test]
fn backward_deletions_prepend_forward_deletions_append_during_accumulation() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("prefix|suffix");
    // Position cursor at "|"
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    } // Move right 6

    input.handle_input("\x0b"); // Ctrl+K - deletes "|suffix" (forward)
    assert_eq!(input.get_value(), "prefix");

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "prefix|suffix");
}

#[test]
fn alt_d_deletes_word_forward_and_saves_to_kill_ring() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("hello world test");
    input.handle_input("\x01"); // Ctrl+A

    input.handle_input("\x1bd"); // Alt+D - deletes "hello"
    assert_eq!(input.get_value(), " world test");

    input.handle_input("\x1bd"); // Alt+D - deletes " world"
    assert_eq!(input.get_value(), " test");

    // Yank should get accumulated text
    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello world test");
}

#[test]
fn alt_d_preserves_ascii_punctuation_boundaries() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("foo.bar baz");
    input.handle_input("\x01"); // Ctrl+A
    input.handle_input("\x1bd"); // Alt+D - deletes "foo"
    assert_eq!(input.get_value(), ".bar baz");
    input.handle_input("\x1bd"); // Alt+D - deletes "."
    assert_eq!(input.get_value(), "bar baz");
    input.handle_input("\x1bd"); // Alt+D - deletes "bar"
    assert_eq!(input.get_value(), " baz");
}

#[test]
fn alt_d_handles_unicode_word_boundaries() {
    let mut input = Input::new(InputOptions::default());

    // "你好世界。你好，世界" segments as: 你好|世界|。|你好|，|世界
    input.set_value("你好世界。你好，世界");
    input.handle_input("\x01"); // Ctrl+A
    input.handle_input("\x1bd"); // Alt+D - deletes "你好"
    assert_eq!(input.get_value(), "世界。你好，世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "世界"
    assert_eq!(input.get_value(), "。你好，世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "。"
    assert_eq!(input.get_value(), "你好，世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "你好"
    assert_eq!(input.get_value(), "，世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "，"
    assert_eq!(input.get_value(), "世界");
    input.handle_input("\x1bd"); // Alt+D - deletes "世界"
    assert_eq!(input.get_value(), "");
}

#[test]
fn handles_yank_in_middle_of_text() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("word");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "word"
    input.set_value("hello world");
    // Move to middle (after "hello ")
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x19"); // Ctrl+Y
    assert_eq!(input.get_value(), "hello wordworld");
}

#[test]
fn handles_yank_pop_in_middle_of_text() {
    let mut input = Input::new(InputOptions::default());

    // Create two kill ring entries
    input.set_value("FIRST");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "FIRST"
    input.set_value("SECOND");
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("\x17"); // Ctrl+W - deletes "SECOND"

    // Set up "hello world" and position cursor after "hello "
    input.set_value("hello world");
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x19"); // Ctrl+Y - yanks "SECOND"
    assert_eq!(input.get_value(), "hello SECONDworld");

    input.handle_input("\x1by"); // Alt+Y - replaces with "FIRST"
    assert_eq!(input.get_value(), "hello FIRSTworld");
}

#[test]
fn does_nothing_when_undo_stack_is_empty() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "");
}

#[test]
fn coalesces_consecutive_word_characters_into_one_undo_unit() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input(" ");
    input.handle_input("w");
    input.handle_input("o");
    input.handle_input("r");
    input.handle_input("l");
    input.handle_input("d");
    assert_eq!(input.get_value(), "hello world");

    // Undo removes " world"
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello");

    // Undo removes "hello"
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "");
}

#[test]
fn undoes_spaces_one_at_a_time() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input(" ");
    input.handle_input(" ");
    assert_eq!(input.get_value(), "hello  ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes second " "
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes first " "
    assert_eq!(input.get_value(), "hello");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo) - removes "hello"
    assert_eq!(input.get_value(), "");
}

#[test]
fn undoes_backspace() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input("\x7f"); // Backspace
    assert_eq!(input.get_value(), "hell");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello");
}

#[test]
fn undoes_forward_delete() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input("\x01"); // Ctrl+A - go to start
    input.handle_input("\x1b[C"); // Right arrow
    input.handle_input("\x1b[3~"); // Delete key
    assert_eq!(input.get_value(), "hllo");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello");
}

#[test]
fn undoes_ctrl_w_delete_word_backward() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input(" ");
    input.handle_input("w");
    input.handle_input("o");
    input.handle_input("r");
    input.handle_input("l");
    input.handle_input("d");
    assert_eq!(input.get_value(), "hello world");

    input.handle_input("\x17"); // Ctrl+W
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undoes_ctrl_k_delete_to_line_end() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input(" ");
    input.handle_input("w");
    input.handle_input("o");
    input.handle_input("r");
    input.handle_input("l");
    input.handle_input("d");
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x0b"); // Ctrl+K
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undoes_ctrl_u_delete_to_line_start() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input(" ");
    input.handle_input("w");
    input.handle_input("o");
    input.handle_input("r");
    input.handle_input("l");
    input.handle_input("d");
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..6 {
        input.handle_input("\x1b[C");
    }

    input.handle_input("\x15"); // Ctrl+U
    assert_eq!(input.get_value(), "world");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undoes_yank() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("h");
    input.handle_input("e");
    input.handle_input("l");
    input.handle_input("l");
    input.handle_input("o");
    input.handle_input(" ");
    input.handle_input("\x17"); // Ctrl+W - delete "hello "
    input.handle_input("\x19"); // Ctrl+Y - yank
    assert_eq!(input.get_value(), "hello ");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "");
}

#[test]
fn undoes_paste_atomically() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("hello world");
    input.handle_input("\x01"); // Ctrl+A
    for _ in 0..5 {
        input.handle_input("\x1b[C");
    }

    // Simulate bracketed paste
    input.handle_input("\x1b[200~beep boop\x1b[201~");
    assert_eq!(input.get_value(), "hellobeep boop world");

    // Single undo should restore entire pre-paste state
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn undoes_alt_d_delete_word_forward() {
    let mut input = Input::new(InputOptions::default());

    input.set_value("hello world");
    input.handle_input("\x01"); // Ctrl+A

    input.handle_input("\x1bd"); // Alt+D - deletes "hello"
    assert_eq!(input.get_value(), " world");

    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "hello world");
}

#[test]
fn cursor_movement_starts_new_undo_unit() {
    let mut input = Input::new(InputOptions::default());

    input.handle_input("a");
    input.handle_input("b");
    input.handle_input("c");
    input.handle_input("\x01"); // Ctrl+A - movement breaks coalescing
    input.handle_input("\x05"); // Ctrl+E
    input.handle_input("d");
    input.handle_input("e");
    assert_eq!(input.get_value(), "abcde");

    // Undo removes "de" (typed after movement)
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "abc");

    // Undo removes "abc"
    input.handle_input("\x1b[45;5u"); // Ctrl+- (undo)
    assert_eq!(input.get_value(), "");
}
