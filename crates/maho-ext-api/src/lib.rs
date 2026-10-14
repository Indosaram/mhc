//! Native, statically registered extension contracts. No host or interactive dependency.
pub mod types;
pub mod mcp_wire;
pub mod process_env;
pub use types::*;
pub use maho_tui::utils::wrap_text_with_ansi as notice_wrap_text;

/// Write one diagnostic line to the process stderr sink. While the interactive TUI owns the
/// terminal this lands in the hidden debug log instead of corrupting the rendered frame.
pub fn diagnostic(message: &str) {
    maho_tui::process_stdio::stderr_write(&format!("{message}\n"));
}
pub fn notice_background(text: &str, width: usize, background: &str) -> String {
    let padding = width.saturating_sub(maho_tui::utils::visible_width(text));
    let padded = format!("{text}{}", " ".repeat(padding));
    maho_tui::utils::apply_background_to_line(&padded, width, &|line| format!("{background}{line}\x1b[49m"))
}

