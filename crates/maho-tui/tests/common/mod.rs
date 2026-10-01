#![allow(dead_code)]
//! Shared helpers for the todo 8 editor tests (senpi `test/test-themes.ts` + `VirtualTerminal`).

use std::cell::Cell;
use std::rc::Rc;

use maho_tui::components::editor::{Editor, EditorOptions, EditorTheme};
use maho_tui::components::select_list::SelectListTheme;

/// Host seam replacing senpi's `TUI` for editor tests: fixed terminal rows, render counter.
pub struct TestHost {
    pub rows: usize,
    renders: Cell<usize>,
}

impl TestHost {
    pub fn new(rows: usize) -> Rc<Self> {
        Rc::new(Self {
            rows,
            renders: Cell::new(0),
        })
    }

    pub fn render_count(&self) -> usize {
        self.renders.get()
    }
}

impl maho_tui::components::editor::EditorTuiHost for TestHost {
    fn request_render(&self) {
        self.renders.set(self.renders.get() + 1);
    }

    fn terminal_rows(&self) -> usize {
        self.rows
    }
}

fn identity(text: &str) -> String {
    text.to_string()
}

/// senpi `defaultSelectListTheme` with styling stripped so assertions can match plain text.
pub fn plain_select_list_theme() -> SelectListTheme {
    SelectListTheme {
        selected_prefix: Rc::new(identity),
        selected_text: Rc::new(identity),
        description: Rc::new(identity),
        scroll_info: Rc::new(identity),
        no_match: Rc::new(identity),
        render_row: None,
    }
}

pub fn plain_editor_theme() -> EditorTheme {
    EditorTheme {
        border_color: Rc::new(identity),
        mention: None,
        select_list: plain_select_list_theme(),
    }
}

pub fn create_editor(rows: usize) -> (Editor, Rc<TestHost>) {
    create_editor_with_theme(plain_editor_theme(), rows)
}

pub fn create_editor_with_theme(theme: EditorTheme, rows: usize) -> (Editor, Rc<TestHost>) {
    let host = TestHost::new(rows);
    let editor = Editor::new(host.clone(), theme, EditorOptions::default());
    (editor, host)
}

/// senpi `Array.from({ length: lines }, (_, i) => `${prefix}-${i + 1}`).join("\n")`.
pub fn large_paste(prefix: &str, lines: usize) -> String {
    (1..=lines)
        .map(|index| format!("{prefix}-{index}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// senpi `editor.handleInput(`\x1b[200~${content}\x1b[201~`)`; returns the raw marker text.
pub fn paste(editor: &mut Editor, content: &str) -> String {
    editor.handle_editor_input(&format!("\x1b[200~{content}\x1b[201~"));
    editor.get_text()
}
