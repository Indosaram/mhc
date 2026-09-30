//! Headless editor demo for pty QA: feeds raw stdin bytes to the `Editor` and prints the
//! rendered frame after every key, so a pty driver can assert on the visible screen.

use std::io::{Read, Write};
use std::rc::Rc;

use maho_tui::autocomplete::{CombinedAutocompleteProvider, CommandSpec};
use maho_tui::components::editor::{
    Editor, EditorOptions, EditorTheme, EditorTuiHost,
};
use maho_tui::components::select_list::SelectListTheme;
use maho_tui::utils::strip_terminal_sequences;

/// Split raw input into key tokens: one character each, escape sequences kept whole.
fn tokenize_input(data: &str) -> Vec<String> {
    let chars: Vec<char> = data.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\x1b' {
            let start = index;
            index += 1;
            if index < chars.len() && chars[index] == '[' {
                index += 1;
                while index < chars.len() && !('\u{40}'..='\u{7e}').contains(&chars[index]) {
                    index += 1;
                }
                if index < chars.len() {
                    index += 1;
                }
            } else if index < chars.len() {
                index += 1;
            }
            tokens.push(chars[start..index].iter().collect());
        } else {
            tokens.push(chars[index].to_string());
            index += 1;
        }
    }
    tokens
}

struct DemoHost {
    rows: usize,
}

impl EditorTuiHost for DemoHost {
    fn request_render(&self) {}
    fn terminal_rows(&self) -> usize {
        self.rows
    }
}

fn identity(text: &str) -> String {
    text.to_string()
}

fn main() {
    let rows = std::env::var("EDITOR_DEMO_ROWS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24usize);
    let width = std::env::var("EDITOR_DEMO_WIDTH")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(80usize);

    let theme = EditorTheme {
        border_color: Rc::new(identity),
        mention: None,
        select_list: SelectListTheme {
            selected_prefix: Rc::new(|text: &str| format!("> {text}")),
            selected_text: Rc::new(identity),
            description: Rc::new(identity),
            scroll_info: Rc::new(identity),
            no_match: Rc::new(identity),
            render_row: None,
        },
    };

    let mut editor = Editor::new(
        Rc::new(DemoHost { rows }),
        theme,
        EditorOptions::default(),
    );
    let provider = CombinedAutocompleteProvider::new(
        vec![
            CommandSpec::with_description("model", "Select a model"),
            CommandSpec::with_description("help", "Show help"),
        ],
        "/tmp",
        None,
    );
    editor.set_autocomplete_provider(Rc::new(std::cell::RefCell::new(provider)));

    let mut stdout = std::io::stdout();
    let mut stdin = std::io::stdin();
    let mut buffer = [0u8; 4096];

    loop {
        let read = match stdin.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => break,
        };
        let chunk = String::from_utf8_lossy(&buffer[..read]).to_string();
        // A real terminal delivers one key at a time through the stdin buffer; split the read
        // the same way so per-character autocomplete triggers fire.
        for token in tokenize_input(&chunk) {
            editor.handle_editor_input(&token);
        }

        let frame: Vec<String> = editor
            .render_editor(width)
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect();
        let _ = write!(stdout, "\x1b[2J\x1b[H{}", frame.join("\r\n"));
        let _ = stdout.flush();
    }
}
