//! Port of interactive/extension-error-format.ts.

use maho_ext_api::types::RUNTIME_EXTENSION_PATH;

/// Remove OSC, CSI, and C0/C1 controls before entering ANSI-preserving rows.
pub fn sanitize_tui_error_message(value: &str) -> String {
    let mut chars = value.chars().peekable();
    let mut output = String::new();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&']') {
            chars.next();
            while let Some(next) = chars.next() {
                if next == '\u{7}' || next == '\u{9c}' {
                    break;
                }
                if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
            continue;
        }
        if character == '\u{9b}' || (character == '\u{1b}' && chars.peek() == Some(&'[')) {
            if character == '\u{1b}' {
                chars.next();
            }
            // Consume a CSI only when its parameter/intermediate bytes end in a final byte.
            let mut probe = chars.clone();
            while probe.peek().is_some_and(|c| ('0'..='?').contains(c)) {
                probe.next();
            }
            while probe.peek().is_some_and(|c| (' '..='/').contains(c)) {
                probe.next();
            }
            if probe.peek().is_some_and(|c| ('@'..='~').contains(c)) {
                probe.next();
                chars = probe;
            } else if character == '\u{1b}' {
                output.push('[');
            }
            continue;
        }
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            output.push('\n');
        } else if matches!(character, '\0'..='\u{8}' | '\u{b}'..='\u{1f}' | '\u{7f}'..='\u{9f}') {
            continue;
        } else if matches!(character, ' ' | '\t') {
            if !output.ends_with(' ') {
                output.push(' ');
            }
        } else {
            output.push(character);
        }
    }
    output
}

pub fn format_extension_error_headline(path: &str, event: Option<&str>, error: &str) -> String {
    let message = sanitize_tui_error_message(error);
    if path == RUNTIME_EXTENSION_PATH {
        match event.filter(|event| !event.is_empty()) {
            Some(event) => format!("Runtime error ({}): {message}", sanitize_tui_error_message(event)),
            None => format!("Runtime error: {message}"),
        }
    } else {
        format!("Extension \"{}\" error: {message}", sanitize_tui_error_message(path))
    }
}
