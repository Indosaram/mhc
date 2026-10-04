use std::{cell::RefCell, rc::Rc, sync::Arc};
use maho_tui::{components::editor::{Editor, EditorOptions, EditorTheme, EditorTuiHost}, editor_component::EditorComponent, image_markers::EditorImageState, keybindings::KeybindingsManager, paste_markers::EditorPasteState, tui::{Component, Focusable}, utils::{truncate_to_width, visible_width}};
use super::status_indicator::StatusIndicator;

pub type ExtensionShortcut = Box<dyn FnMut(&str) -> bool>;

#[derive(Default)]
pub struct CustomEditorOptions { pub editor: EditorOptions, pub embed_working_status: bool }

pub struct CustomEditor {
    pub editor: Editor,
    keybindings: Arc<KeybindingsManager>,
    configured_padding_x: usize,
    prompt_padding_x: usize,
    working_status: Option<Rc<RefCell<StatusIndicator>>>,
    reply_label: Option<String>,
    pub embed_working_status: bool,
    pub action_handlers: Vec<(String, Box<dyn FnMut()>)>,
    pub on_escape: Option<Box<dyn FnMut()>>,
    pub on_ctrl_d: Option<Box<dyn FnMut()>>,
    pub on_paste_image: Option<Box<dyn FnMut()>>,
    pub on_extension_shortcut: Option<ExtensionShortcut>,
}

impl CustomEditor {
    pub fn new(host: Rc<dyn EditorTuiHost>, theme: EditorTheme, keybindings: Arc<KeybindingsManager>, mut options: CustomEditorOptions) -> Self {
        let configured_padding_x = options.editor.padding_x.unwrap_or(0);
        let prompt_padding_x = configured_padding_x.max(2);
        options.editor.padding_x = Some(prompt_padding_x);
        Self {
            editor: Editor::new(host, theme, options.editor), keybindings, configured_padding_x, prompt_padding_x,
            working_status: None, reply_label: None, embed_working_status: options.embed_working_status,
            action_handlers: Vec::new(), on_escape: None, on_ctrl_d: None, on_paste_image: None, on_extension_shortcut: None,
        }
    }
    pub fn get_padding_x(&self) -> usize { self.configured_padding_x }
    pub fn set_padding_x(&mut self, padding: usize) {
        self.configured_padding_x = padding;
        self.prompt_padding_x = padding.max(2);
        self.editor.set_padding_x(self.prompt_padding_x);
    }
    pub fn set_working_status_indicator(&mut self, indicator: Option<Rc<RefCell<StatusIndicator>>>) { self.working_status = indicator; }
    pub fn set_keybindings(&mut self, keybindings: Arc<KeybindingsManager>) { self.keybindings = keybindings; }
    pub fn set_reply_label(&mut self, label: Option<String>) { self.reply_label = label; }
    pub fn on_action(&mut self, action: &str, handler: Box<dyn FnMut()>) {
        if let Some((_, current)) = self.action_handlers.iter_mut().find(|(id, _)| id == action) { *current = handler; }
        else { self.action_handlers.push((action.into(), handler)); }
    }
    fn render_top_border(&mut self, width: usize, hidden: usize) -> String {
        let border = &self.editor.border_color;
        if let Some(label) = self.reply_label.as_deref().filter(|label| !label.is_empty()) && width >= 5 {
            let label = truncate_to_width(label, width - 4, "…", false);
            return format!("{}{label}{}", border("── "), border(&format!(" {}", "─".repeat(width.saturating_sub(visible_width(&label) + 4)))));
        }
        let Some(indicator) = self.working_status.as_ref().filter(|_| self.embed_working_status && width > 0) else { return self.editor.render_top_border(width, hidden); };
        let mut indicator = indicator.borrow_mut();
        let mut status = indicator.render_in_border(width.saturating_sub(5).max(1));
        let mut status_width = visible_width(&status);
        if status_width == 0 { return self.editor.render_top_border(width, hidden); }
        let overflow = (hidden > 0).then(|| format!(" ↑ {hidden} more "));
        let overflow_width = overflow.as_ref().map_or(0, |label| visible_width(label));
        let overflow_start = width.saturating_sub(overflow_width) / 2;
        let fits = |status_width| overflow.is_some() && overflow_width + 2 <= width && overflow_start > 3 + status_width + 1;
        if overflow.is_some() && !fits(status_width) { status = indicator.render_spinner_in_border(width); status_width = visible_width(&status); }
        if fits(status_width) {
            let suffix = format!(" {}{}{}", "─".repeat(overflow_start - (3 + status_width + 1)), overflow.unwrap_or_default(), "─".repeat(width - overflow_start - overflow_width));
            return format!("{}{status}{}", border("── "), border(&suffix));
        }
        if width >= status_width + 5 { return format!("{}{status}{}", border("── "), border(&format!(" {}", "─".repeat(width - status_width - 4)))); }
        status = indicator.render_spinner_in_border(width);
        status_width = visible_width(&status);
        let prefix = width.saturating_sub(status_width).min(3);
        format!("{}{status}{}", border(&"─".repeat(prefix)), border(&"─".repeat(width.saturating_sub(prefix + status_width))))
    }
}

impl Component for CustomEditor {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = self.editor.render(width);
        let hidden = lines.first().and_then(|line| line.split(" ↑ ").nth(1)).and_then(|text| text.split_whitespace().next()).and_then(|value| value.parse().ok()).unwrap_or(0);
        if let Some(top) = lines.first_mut() { *top = self.render_top_border(width, hidden); }
        if width < 5 || lines.first().is_some_and(|line| line.contains('↑')) { return lines; }
        if let Some(line) = lines.get_mut(1) {
            let offset = line.char_indices().nth(self.prompt_padding_x).map_or(line.len(), |(offset, _)| offset);
            *line = format!("{} {}", (self.editor.border_color)("❯"), &line[offset..]);
        }
        lines
    }
    fn handle_input(&mut self, data: &str) {
        if self.on_extension_shortcut.as_mut().is_some_and(|callback| callback(data)) { return; }
        if self.keybindings.matches(data, "app.clipboard.pasteImage") { if let Some(callback) = &mut self.on_paste_image { callback(); } return; }
        if self.keybindings.matches(data, "app.interrupt") {
            if !self.editor.is_showing_autocomplete() {
                let handler = self.on_escape.as_mut().or_else(|| self.action_handlers.iter_mut().find(|(id, _)| id == "app.interrupt").map(|(_, handler)| handler));
                if let Some(handler) = handler { handler(); return; }
            }
            self.editor.handle_input(data); return;
        }
        if self.keybindings.matches(data, "app.exit") && self.editor.get_text().is_empty() {
            let handler = self.on_ctrl_d.as_mut().or_else(|| self.action_handlers.iter_mut().find(|(id, _)| id == "app.exit").map(|(_, handler)| handler));
            if let Some(handler) = handler { handler(); }
            return;
        }
        if self.keybindings.matches(data, "tui.editor.historyPrevious") || self.keybindings.matches(data, "tui.editor.historyNext") { self.editor.handle_input(data); return; }
        for (action, handler) in &mut self.action_handlers {
            if action != "app.interrupt" && action != "app.exit" && self.keybindings.matches(data, action) { handler(); return; }
        }
        self.editor.handle_input(data);
    }
    fn has_input_handler(&self) -> bool { true }
    fn invalidate(&mut self) { self.editor.invalidate(); }
}

impl Focusable for CustomEditor {
    fn focused(&self) -> bool { self.editor.focused() }
    fn set_focused(&mut self, focused: bool) { self.editor.set_focused(focused); }
}

impl EditorComponent for CustomEditor {
    fn get_text(&self) -> String { self.editor.get_text() }
    fn set_text(&mut self, text: &str) { self.editor.set_text(text); }
    fn add_to_history(&mut self, text: &str) { self.editor.add_to_history(text); }
    fn insert_text_at_cursor(&mut self, text: &str) { self.editor.insert_text_at_cursor(text); }
    fn get_expanded_text(&self) -> Option<String> { Some(self.editor.get_expanded_text()) }
    fn get_paste_state(&self) -> Option<EditorPasteState> { Some(self.editor.get_paste_state()) }
    fn set_paste_state(&mut self, state: &EditorPasteState) { self.editor.set_paste_state(state); }
    fn insert_image_marker(&mut self) -> Option<u64> { Some(self.editor.insert_image_marker()) }
    fn get_image_marker_state(&self) -> Option<EditorImageState> { Some(self.editor.get_image_marker_state()) }
    fn set_image_marker_state(&mut self, state: &EditorImageState) { self.editor.set_image_marker_state(state); }
    fn set_padding_x(&mut self, padding: usize) { CustomEditor::set_padding_x(self, padding); }
    fn set_autocomplete_max_visible(&mut self, visible: usize) { self.editor.set_autocomplete_max_visible(visible); }
    fn set_autocomplete_provider(&mut self, provider: Box<dyn maho_tui::autocomplete::AutocompleteProvider>) {
        self.editor.set_autocomplete_provider(Rc::new(RefCell::new(BoxedAutocomplete(provider))));
    }
}

struct BoxedAutocomplete(Box<dyn maho_tui::autocomplete::AutocompleteProvider>);
impl maho_tui::autocomplete::AutocompleteProvider for BoxedAutocomplete {
    fn trigger_characters(&self) -> Vec<String> { self.0.trigger_characters() }
    fn get_suggestions(&mut self, lines: &[String], cursor_line: usize, cursor_col: usize, force: bool) -> Option<maho_tui::autocomplete::AutocompleteSuggestions> { self.0.get_suggestions(lines, cursor_line, cursor_col, force) }
    fn apply_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize, item: &maho_tui::autocomplete::AutocompleteItem, prefix: &str) -> maho_tui::autocomplete::ApplyCompletionResult { self.0.apply_completion(lines, cursor_line, cursor_col, item, prefix) }
    fn get_mention_ranges(&self, text: &str) -> Vec<maho_tui::autocomplete::MentionRange> { self.0.get_mention_ranges(text) }
    fn should_trigger_file_completion(&self, lines: &[String], cursor_line: usize, cursor_col: usize) -> bool { self.0.should_trigger_file_completion(lines, cursor_line, cursor_col) }
}
