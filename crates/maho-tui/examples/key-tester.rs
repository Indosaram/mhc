//! Port of senpi \`packages/tui/test/key-tester.ts\`: logs raw key input and the negotiated
//! keyboard protocol. senpi renders through TuiMainScreen (plan todo 7); until that lands this
//! example redraws the full frame directly on the ProcessTerminal.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::rc::Rc;

use maho_tui::keys::matches_key;
use maho_tui::terminal::{ProcessTerminal, Terminal};
use maho_tui::utils::{truncate_to_width, visible_width};

const MAX_LINES: usize = 20;

fn log_line(data: &str) -> String {
    let hex = data.bytes().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    });
    // JS charCodeAt over UTF-16 code units.
    let char_codes = data
        .encode_utf16()
        .map(|u| u.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let repr = data
        .replace('\x1b', "\\x1b")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\x7f', "\\x7f");
    format!("Hex: {hex:<20} | Chars: [{char_codes:<15}] | Repr: \"{repr}\"")
}

fn fit(line: &str, width: usize) -> String {
    let truncated = truncate_to_width(line, width, "...", false);
    let pad = width.saturating_sub(visible_width(&truncated));
    format!("{truncated}{}", " ".repeat(pad))
}

fn render(log: &[String], protocol: &str, width: usize) -> Vec<String> {
    let rule = "=".repeat(width);
    let mut lines = vec![
        rule.clone(),
        fit(
            "Key Code Tester - Press keys to see their codes (Ctrl+C to exit)",
            width,
        ),
        fit(&format!("Protocol: {protocol}"), width),
        rule.clone(),
        String::new(),
    ];
    lines.extend(log.iter().map(|entry| fit(entry, width)));
    while lines.len() < 25 {
        lines.push(" ".repeat(width));
    }
    lines.push(rule.clone());
    for footer in [
        "Test these:",
        "  - Shift + Enter (should show: \\x1b[13;2u with Kitty protocol)",
        "  - Alt/Option + Enter",
        "  - Option/Alt + Backspace",
        "  - Cmd/Ctrl + Backspace",
        "  - Regular Backspace",
    ] {
        lines.push(fit(footer, width));
    }
    lines.push(rule);
    lines
}

fn protocol_name(terminal: &ProcessTerminal) -> &'static str {
    if terminal.kitty_protocol_active() {
        "kitty"
    } else if terminal.modify_other_keys_active() {
        "modifyOtherKeys"
    } else {
        "legacy"
    }
}

fn main() {
    let log = Rc::new(RefCell::new(Vec::<String>::new()));
    let exit = Rc::new(RefCell::new(false));
    let mut terminal = ProcessTerminal::default();
    let (log_in, exit_in) = (Rc::clone(&log), Rc::clone(&exit));
    terminal.start(
        Box::new(move |data| {
            // Ctrl+C (raw or Kitty protocol) exits; raw mode turns SIGINT into input.
            if matches_key(data, "ctrl+c") {
                *exit_in.borrow_mut() = true;
                return;
            }
            let mut log = log_in.borrow_mut();
            log.push(log_line(data));
            if log.len() > MAX_LINES {
                log.remove(0);
            }
        }),
        Box::new(|| {}),
    );
    terminal.hide_cursor();
    let mut previous: Vec<String> = Vec::new();
    while !*exit.borrow() {
        let width = usize::from(terminal.columns());
        let frame = render(&log.borrow(), protocol_name(&terminal), width);
        if frame != previous {
            terminal.write(&format!(
                "\x1b[?2026h\x1b[H{}\x1b[J\x1b[?2026l",
                frame.join("\r\n")
            ));
            previous = frame;
        }
        // Protocol negotiation completes asynchronously; re-render at least every 100ms.
        if let Err(error) = terminal.pump(100) {
            eprintln!("stdin error: {error}");
            break;
        }
    }
    terminal.show_cursor();
    terminal.drain_input(1000, 50);
    if let Err(error) = terminal.stop() {
        eprintln!("terminal restore failed: {error}");
    }
    println!("\nExiting...");
}
