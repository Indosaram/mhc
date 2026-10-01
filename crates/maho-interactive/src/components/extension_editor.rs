//! Port of components/extension-editor.ts.
//!
//! Multi-line editor dialog for extensions, with Ctrl+G for an external editor. senpi reaches
//! `editInExternalEditor` from `../external-editor.ts` (plan todo 35 owns that module), so this
//! port takes the external-editor round trip as a host callback the todo-35 mode wires up.

use std::rc::Rc;

use maho_tui::components::editor::{Editor, EditorOptions, EditorTheme, EditorTuiHost};
use maho_tui::components::text::Text;
use maho_tui::keybindings::KeybindingsManager;
use maho_tui::tui::{Component, Focusable};
use std::sync::Arc;

use super::keybinding_hints::key_hint;
use super::theme_selector::{dynamic_border, select_list_theme};
use crate::theme::theme::{Theme, ThemeColor};

/// senpi's `getEditorTheme()`.
pub fn editor_theme(theme: &Theme) -> EditorTheme {
    let border = theme.clone();
    let mention = theme.clone();
    EditorTheme {
        border_color: Rc::new(move |text: &str| border.fg(ThemeColor::BorderMuted, text)),
        mention: Some(Rc::new(move |text: &str| {
            mention.fg(ThemeColor::SkillMention, &format!("\u{1b}[1m{text}\u{1b}[22m"))
        })),
        select_list: select_list_theme(theme),
    }
}

/// Host seam for the external-editor round trip (senpi's `editInExternalEditor`).
pub type ExternalEdit = Box<dyn FnMut(&str) -> Option<String>>;

pub struct ExtensionEditorComponent {
    theme: Theme,
    editor: Editor,
    title: String,
    external_edit: Option<ExternalEdit>,
    external_editor_command: String,
    focused: bool,
    on_cancel: Box<dyn FnMut()>,
    keybindings: Arc<KeybindingsManager>,
}

impl ExtensionEditorComponent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &Theme,
        host: Rc<dyn EditorTuiHost>,
        keybindings: Arc<KeybindingsManager>,
        title: &str,
        prefill: Option<&str>,
        on_submit: Box<dyn FnMut(&str)>,
        on_cancel: Box<dyn FnMut()>,
        options: EditorOptions,
        external_edit: Option<ExternalEdit>,
        external_editor_command: Option<String>,
    ) -> Self {
        let external_editor_command = external_editor_command
            .or_else(|| std::env::var("VISUAL").ok())
            .or_else(|| std::env::var("EDITOR").ok())
            .unwrap_or_else(|| {
                if cfg!(target_os = "windows") {
                    "notepad".into()
                } else {
                    "nano".into()
                }
            });
        let mut editor = Editor::new(host, editor_theme(theme), options);
        if let Some(prefill) = prefill {
            editor.set_text(prefill);
        }
        editor.on_submit = Some(on_submit);
        Self {
            theme: theme.clone(),
            editor,
            title: title.to_owned(),
            external_edit,
            external_editor_command,
            focused: false,
            on_cancel,
            keybindings,
        }
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    pub fn external_editor_command(&self) -> &str {
        &self.external_editor_command
    }

    pub fn title(&self) -> &str {
        &self.title
    }
}

impl Component for ExtensionEditorComponent {
    fn render(&mut self, width: usize) -> Vec<String> {
        let mut lines = vec![dynamic_border(&self.theme, ThemeColor::Border, width), String::new()];
        let title = self.theme.fg(ThemeColor::Accent, &self.title.clone());
        lines.extend(Text::with_padding(title, 1, 0).render(width));
        lines.push(String::new());
        lines.extend(self.editor.render(width));
        lines.push(String::new());
        let hint = format!(
            "{}  {}  {}  {}",
            key_hint("tui.select.confirm", "submit", &self.theme),
            key_hint("tui.input.newLine", "newline", &self.theme),
            key_hint("tui.select.cancel", "cancel", &self.theme),
            key_hint("app.editor.external", "external editor", &self.theme)
        );
        lines.extend(Text::with_padding(hint, 1, 0).render(width));
        lines.push(String::new());
        lines.push(dynamic_border(&self.theme, ThemeColor::Border, width));
        lines
    }

    fn handle_input(&mut self, data: &str) {
        if self.keybindings.matches(data, "tui.select.cancel") {
            (self.on_cancel)();
            return;
        }
        if self.keybindings.matches(data, "app.editor.external") {
            let content = self.editor.get_text();
            if let Some(external_edit) = &mut self.external_edit
                && let Some(updated) = external_edit(&content)
            {
                self.editor.set_text(&updated);
            }
            return;
        }
        self.editor.handle_input(data);
    }

    fn has_input_handler(&self) -> bool {
        true
    }

    fn focusable_get(&self) -> Option<bool> {
        Some(self.focused)
    }

    fn focusable_set(&mut self, focused: bool) {
        self.focused = focused;
        self.editor.set_focused(focused);
    }

    fn invalidate(&mut self) {
        self.editor.invalidate();
    }
}

impl Focusable for ExtensionEditorComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, value: bool) {
        self.focused = value;
        self.editor.set_focused(value);
    }
}
