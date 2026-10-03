use super::clipboard_command::{ClipboardCommandOptions, run_clipboard_command};
fn present(name: &str) -> bool { std::env::var_os(name).is_some_and(|value| !value.is_empty()) }
fn commands(read: bool) -> Vec<(&'static str, Vec<&'static str>)> {
    let mut commands = Vec::new();
    if cfg!(target_os = "linux") {
        if present("TERMUX_VERSION") { commands.push((if read { "termux-clipboard-get" } else { "termux-clipboard-set" }, Vec::new())); }
        if present("WAYLAND_DISPLAY") { commands.push(if read { ("wl-paste", vec!["--no-newline", "--type", "text"]) } else { ("wl-copy", Vec::new()) }); }
        if present("DISPLAY") {
            commands.push(("xclip", if read { vec!["-selection", "clipboard", "-out"] } else { vec!["-selection", "clipboard"] }));
            commands.push(("xsel", vec!["--clipboard", if read { "--output" } else { "--input" }]));
        }
    } else if !read { commands.push((if cfg!(target_os = "macos") { "pbcopy" } else { "clip" }, Vec::new())); }
    commands
}
pub async fn read_clipboard_text() -> Option<String> {
    for (command, args) in commands(true) {
        if let Some(bytes) = run_clipboard_command(command, &args, ClipboardCommandOptions { timeout_ms: Some(5000), ..Default::default() }).await {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return (!text.is_empty()).then_some(text);
        }
    }
    maho_tui::native_platform::get_native_clipboard()?.get_text().ok().flatten().filter(|text| !text.is_empty())
}
pub fn osc52_sequence(text: &str) -> Option<String> {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    (encoded.len() <= 100_000).then(|| format!("\x1b]52;c;{encoded}\x07"))
}
pub async fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut copied = false;
    if !cfg!(target_os = "linux") && let Some(clipboard) = maho_tui::native_platform::get_native_clipboard() {
        copied = clipboard.set_text(text).is_some_and(|result| result.is_ok());
    }
    if !copied {
        for (command, args) in commands(false) {
            if run_clipboard_command(command, &args, ClipboardCommandOptions { input: Some(text), timeout_ms: Some(5000), ..Default::default() }).await.is_some() { copied = true; break; }
        }
    }
    if (["SSH_CONNECTION", "SSH_CLIENT", "MOSH_CONNECTION"].into_iter().any(present) || !copied)
        && let Some(sequence) = osc52_sequence(text) {
        use std::io::Write;
        std::io::stdout().lock().write_all(sequence.as_bytes()).map_err(|error| error.to_string())?;
        copied = true;
    }
    if copied { Ok(()) } else { Err("Failed to copy to clipboard".to_owned()) }
}
