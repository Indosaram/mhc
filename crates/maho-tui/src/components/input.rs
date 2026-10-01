//! Port of senpi `packages/tui/src/components/input.ts`.
//!
//! senpi's `Input` composes the shared `KillRing`/`UndoStack` classes from
//! `kill-ring.ts`/`undo-stack.ts`. Those two Rust modules are reserved for plan todo 8 (they
//! also back the multiline editor, which is out of this todo's scope), so this port carries
//! equivalent minimal ring/stack state privately inside `Input` instead of importing the
//! shared modules. The behavior matches `KillRing`/`UndoStack` exactly; only the code's
//! location differs, and it must be deleted in favor of the shared modules once todo 8 lands.

use crate::keybindings::get_keybindings;
use crate::keys::decode_kitty_printable;
use crate::tui::{Component, Focusable, TuiMouseButton, TuiMouseEvent, TuiMouseEventResult, TuiMouseEventType, CURSOR_MARKER};
use crate::utils::{graphemes, is_whitespace_char, slice_by_column, truncate_to_width, visible_width};
use crate::word_navigation::{find_word_backward, find_word_forward, WordNavigationOptions};

#[derive(Clone)]
struct InputState {
    value: String,
    cursor: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LastAction {
    Kill,
    Yank,
    TypeWord,
}

pub type PlaceholderStyle = std::rc::Rc<dyn Fn(&str) -> String>;

#[derive(Default)]
pub struct InputOptions {
    pub prompt: Option<String>,
    pub placeholder: Option<String>,
    pub placeholder_style: Option<PlaceholderStyle>,
}

pub type OnSubmit = Box<dyn FnMut(&str)>;
pub type OnEscape = Box<dyn FnMut()>;

/// Single-line text input with horizontal scrolling.
pub struct Input {
    value: String,
    cursor: usize,
    prompt: String,
    placeholder: String,
    placeholder_style: PlaceholderStyle,
    rendered_start_column: usize,
    pub on_submit: Option<OnSubmit>,
    pub on_escape: Option<OnEscape>,
    focused: bool,
    paste_buffer: String,
    is_in_paste: bool,
    kill_ring: Vec<String>,
    last_action: Option<LastAction>,
    undo_stack: Vec<InputState>,
}

impl Input {
    pub fn new(options: InputOptions) -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            prompt: options.prompt.unwrap_or_else(|| "> ".to_string()),
            placeholder: options.placeholder.unwrap_or_default(),
            placeholder_style: options.placeholder_style.unwrap_or_else(|| std::rc::Rc::new(|s: &str| s.to_string())),
            rendered_start_column: 0,
            on_submit: None,
            on_escape: None,
            focused: false,
            paste_buffer: String::new(),
            is_in_paste: false,
            kill_ring: Vec::new(),
            last_action: None,
            undo_stack: Vec::new(),
        }
    }

    pub fn get_value(&self) -> &str {
        &self.value
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
        self.cursor = self.cursor.min(self.value.len());
    }

    fn kill_ring_push(&mut self, text: &str, prepend: bool, accumulate: bool) {
        if text.is_empty() {
            return;
        }
        if accumulate
            && let Some(last) = self.kill_ring.pop()
        {
            self.kill_ring.push(if prepend { format!("{text}{last}") } else { format!("{last}{text}") });
            return;
        }
        self.kill_ring.push(text.to_string());
    }

    fn kill_ring_peek(&self) -> Option<&str> {
        self.kill_ring.last().map(String::as_str)
    }

    fn kill_ring_rotate(&mut self) {
        if self.kill_ring.len() > 1
            && let Some(last) = self.kill_ring.pop()
        {
            self.kill_ring.insert(0, last);
        }
    }

    fn last_grapheme_len(text: &str) -> usize {
        graphemes(text).last().map(str::len).unwrap_or(1)
    }

    fn first_grapheme_len(text: &str) -> usize {
        graphemes(text).next().map(str::len).unwrap_or(1)
    }

    fn handle_paste_data(&mut self, mut data: String) {
        if let Some(idx) = data.find("\x1b[200~") {
            self.is_in_paste = true;
            self.paste_buffer.clear();
            data.replace_range(idx..idx + "\x1b[200~".len(), "");
        }
        if self.is_in_paste {
            self.paste_buffer.push_str(&data);
            if let Some(end_index) = self.paste_buffer.find("\x1b[201~") {
                let paste_content = self.paste_buffer[..end_index].to_string();
                self.handle_paste(&paste_content);
                self.is_in_paste = false;
                let remaining = self.paste_buffer[end_index + "\x1b[201~".len()..].to_string();
                self.paste_buffer.clear();
                if !remaining.is_empty() {
                    self.handle_input_impl(remaining);
                }
            }
        }
    }

    fn handle_input_impl(&mut self, data: String) {
        if data.contains("\x1b[200~") || self.is_in_paste {
            self.handle_paste_data(data);
            return;
        }

        let kb = get_keybindings();

        if kb.matches(&data, "tui.select.cancel") {
            if let Some(on_escape) = &mut self.on_escape {
                on_escape();
            }
            return;
        }

        if kb.matches(&data, "tui.editor.undo") {
            self.undo();
            return;
        }

        if kb.matches(&data, "tui.input.submit") || data == "\n" {
            if let Some(on_submit) = &mut self.on_submit {
                let value = self.value.clone();
                on_submit(&value);
            }
            return;
        }

        if kb.matches(&data, "tui.editor.deleteCharBackward") {
            self.handle_backspace();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteCharForward") {
            self.handle_forward_delete();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteWordBackward") {
            self.delete_word_backwards();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteWordForward") {
            self.delete_word_forward();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteToLineStart") {
            self.delete_to_line_start();
            return;
        }

        if kb.matches(&data, "tui.editor.deleteToLineEnd") {
            self.delete_to_line_end();
            return;
        }

        if kb.matches(&data, "tui.editor.yank") {
            self.yank();
            return;
        }

        if kb.matches(&data, "tui.editor.yankPop") {
            self.yank_pop();
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLeft") {
            self.last_action = None;
            if self.cursor > 0 {
                let before_cursor = &self.value[..self.cursor];
                self.cursor -= Self::last_grapheme_len(before_cursor);
            }
            return;
        }

        if kb.matches(&data, "tui.editor.cursorRight") {
            self.last_action = None;
            if self.cursor < self.value.len() {
                let after_cursor = &self.value[self.cursor..];
                self.cursor += Self::first_grapheme_len(after_cursor);
            }
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLineStart") {
            self.last_action = None;
            self.cursor = 0;
            return;
        }

        if kb.matches(&data, "tui.editor.cursorLineEnd") {
            self.last_action = None;
            self.cursor = self.value.len();
            return;
        }

        if kb.matches(&data, "tui.editor.cursorWordLeft") {
            self.move_word_backwards();
            return;
        }

        if kb.matches(&data, "tui.editor.cursorWordRight") {
            self.move_word_forwards();
            return;
        }

        if let Some(kitty_printable) = decode_kitty_printable(&data) {
            self.insert_character(&kitty_printable);
            return;
        }

        let has_control_chars = data.chars().any(|ch| {
            let code = ch as u32;
            code < 32 || code == 0x7f || (0x80..=0x9f).contains(&code)
        });
        if !has_control_chars {
            self.insert_character(&data);
        }
    }

    fn insert_character(&mut self, char: &str) {
        if is_whitespace_char(char) || self.last_action != Some(LastAction::TypeWord) {
            self.push_undo();
        }
        self.last_action = Some(LastAction::TypeWord);
        self.value.insert_str(self.cursor, char);
        self.cursor += char.len();
    }

    fn handle_backspace(&mut self) {
        self.last_action = None;
        if self.cursor > 0 {
            self.push_undo();
            let before_cursor = &self.value[..self.cursor];
            let grapheme_length = Self::last_grapheme_len(before_cursor);
            self.value.replace_range(self.cursor - grapheme_length..self.cursor, "");
            self.cursor -= grapheme_length;
        }
    }

    fn handle_forward_delete(&mut self) {
        self.last_action = None;
        if self.cursor < self.value.len() {
            self.push_undo();
            let after_cursor = &self.value[self.cursor..];
            let grapheme_length = Self::first_grapheme_len(after_cursor);
            self.value.replace_range(self.cursor..self.cursor + grapheme_length, "");
        }
    }

    fn delete_to_line_start(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.push_undo();
        let deleted_text = self.value[..self.cursor].to_string();
        let accumulate = self.last_action == Some(LastAction::Kill);
        self.kill_ring_push(&deleted_text, true, accumulate);
        self.last_action = Some(LastAction::Kill);
        self.value.replace_range(..self.cursor, "");
        self.cursor = 0;
    }

    fn delete_to_line_end(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        self.push_undo();
        let deleted_text = self.value[self.cursor..].to_string();
        let accumulate = self.last_action == Some(LastAction::Kill);
        self.kill_ring_push(&deleted_text, false, accumulate);
        self.last_action = Some(LastAction::Kill);
        self.value.truncate(self.cursor);
    }

    fn delete_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let was_kill = self.last_action == Some(LastAction::Kill);
        self.push_undo();
        let old_cursor = self.cursor;
        self.move_word_backwards();
        let delete_from = self.cursor;
        self.cursor = old_cursor;
        let deleted_text = self.value[delete_from..self.cursor].to_string();
        self.kill_ring_push(&deleted_text, true, was_kill);
        self.last_action = Some(LastAction::Kill);
        self.value.replace_range(delete_from..self.cursor, "");
        self.cursor = delete_from;
    }

    fn delete_word_forward(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        let was_kill = self.last_action == Some(LastAction::Kill);
        self.push_undo();
        let old_cursor = self.cursor;
        self.move_word_forwards();
        let delete_to = self.cursor;
        self.cursor = old_cursor;
        let deleted_text = self.value[self.cursor..delete_to].to_string();
        self.kill_ring_push(&deleted_text, false, was_kill);
        self.last_action = Some(LastAction::Kill);
        self.value.replace_range(self.cursor..delete_to, "");
    }

    fn yank(&mut self) {
        let Some(text) = self.kill_ring_peek().map(str::to_string) else {
            return;
        };
        self.push_undo();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.last_action = Some(LastAction::Yank);
    }

    fn yank_pop(&mut self) {
        if self.last_action != Some(LastAction::Yank) || self.kill_ring.len() <= 1 {
            return;
        }
        self.push_undo();
        let prev_text = self.kill_ring_peek().unwrap_or("").to_string();
        self.value.replace_range(self.cursor - prev_text.len()..self.cursor, "");
        self.cursor -= prev_text.len();
        self.kill_ring_rotate();
        let text = self.kill_ring_peek().unwrap_or("").to_string();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.last_action = Some(LastAction::Yank);
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(InputState { value: self.value.clone(), cursor: self.cursor });
    }

    fn undo(&mut self) {
        let Some(snapshot) = self.undo_stack.pop() else {
            return;
        };
        self.value = snapshot.value;
        self.cursor = snapshot.cursor;
        self.last_action = None;
    }

    fn move_word_backwards(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.last_action = None;
        self.cursor = find_word_backward(&self.value, self.cursor, &WordNavigationOptions::default());
    }

    fn move_word_forwards(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }
        self.last_action = None;
        self.cursor = find_word_forward(&self.value, self.cursor, &WordNavigationOptions::default());
    }

    fn handle_paste(&mut self, pasted_text: &str) {
        self.last_action = None;
        self.push_undo();
        let clean_text = pasted_text.replace(['\r', '\n'], "").replace('\t', "    ");
        self.value.insert_str(self.cursor, &clean_text);
        self.cursor += clean_text.len();
    }
}

impl Focusable for Input {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
    }
}

impl Component for Input {
    fn handle_input(&mut self, data: &str) {
        self.handle_input_impl(data.to_string());
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn handle_mouse(&mut self, event: &TuiMouseEvent) -> Option<TuiMouseEventResult> {
        if event.event_type != TuiMouseEventType::Press || event.button != TuiMouseButton::Left || event.y != 0 {
            return None;
        }
        let visible_column = (event.x - 2).max(0) as usize;
        let target_column = self.rendered_start_column + visible_column;
        let mut current_column = 0usize;
        self.cursor = self.value.len();
        for grapheme in graphemes(&self.value) {
            let index = grapheme.as_ptr() as usize - self.value.as_ptr() as usize;
            let next_column = current_column + visible_width(grapheme);
            if target_column < next_column {
                self.cursor = index;
                break;
            }
            current_column = next_column;
        }
        self.last_action = None;
        Some(TuiMouseEventResult { handled: true, capture: false, focus: true, render: None })
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn invalidate(&mut self) {}

    fn render(&mut self, width: usize) -> Vec<String> {
        let available_width = width as i64 - visible_width(&self.prompt) as i64;
        if available_width <= 0 {
            return vec![truncate_to_width(&self.prompt, width, "", false)];
        }
        let available_width = available_width as usize;

        if self.value.is_empty() && !self.placeholder.is_empty() {
            let placeholder = truncate_to_width(&self.placeholder, available_width, "", false);
            let at_cursor = graphemes(&placeholder).next().unwrap_or(" ").to_string();
            let after_cursor = placeholder[at_cursor.len()..].to_string();
            let marker = if self.focused { CURSOR_MARKER } else { "" };
            let cursor_char = format!("\x1b[7m{}\x1b[27m", (self.placeholder_style)(&at_cursor));
            let text_with_cursor = format!("{marker}{cursor_char}{}", (self.placeholder_style)(&after_cursor));
            let padding = " ".repeat(available_width.saturating_sub(visible_width(&text_with_cursor)));
            return vec![format!("{}{text_with_cursor}{padding}", self.prompt)];
        }

        let mut visible_text;
        let mut cursor_display;
        self.rendered_start_column = 0;
        let total_width = visible_width(&self.value);

        if total_width < available_width {
            visible_text = self.value.clone();
            cursor_display = self.cursor;
        } else {
            let scroll_width = if self.cursor == self.value.len() { available_width.saturating_sub(1) } else { available_width };
            let cursor_col = visible_width(&self.value[..self.cursor]);

            if scroll_width > 0 {
                let half_width = scroll_width / 2;
                let start_col = if cursor_col < half_width {
                    0
                } else if cursor_col > total_width.saturating_sub(half_width) {
                    total_width.saturating_sub(scroll_width)
                } else {
                    cursor_col.saturating_sub(half_width)
                };
                self.rendered_start_column = start_col;
                visible_text = slice_by_column(&self.value, start_col, scroll_width, true);
                let before_cursor = slice_by_column(&self.value, start_col, cursor_col.saturating_sub(start_col), true);
                cursor_display = before_cursor.len();
            } else {
                visible_text = String::new();
                cursor_display = 0;
            }
        }

        if cursor_display > visible_text.len() {
            cursor_display = visible_text.len();
        }
        let at_cursor = graphemes(&visible_text[cursor_display..]).next().unwrap_or(" ").to_string();
        let before_cursor = visible_text[..cursor_display].to_string();
        let after_cursor = visible_text[(cursor_display + at_cursor.len()).min(visible_text.len())..].to_string();
        let _ = &mut visible_text;

        let marker = if self.focused { CURSOR_MARKER } else { "" };
        let cursor_char = format!("\x1b[7m{at_cursor}\x1b[27m");
        let text_with_cursor = format!("{before_cursor}{marker}{cursor_char}{after_cursor}");

        let visual_length = visible_width(&text_with_cursor);
        let padding = " ".repeat(available_width.saturating_sub(visual_length));
        vec![format!("{}{text_with_cursor}{padding}", self.prompt)]
    }
}
