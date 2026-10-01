//! Port of terminal-theme-cache.ts.
use super::theme::TerminalTheme;
use std::path::{Path, PathBuf};
fn hint_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("cache/terminal-theme.json")
}
pub fn read_terminal_theme_hint(agent_dir: &Path) -> Option<TerminalTheme> {
    let data = std::fs::read_to_string(hint_path(agent_dir)).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&data).ok()?;
    serde_json::from_value(parsed.get("terminalTheme")?.clone()).ok()
}
pub fn write_terminal_theme_hint(
    theme: TerminalTheme,
    agent_dir: &Path,
) -> Result<(), std::io::Error> {
    let path = hint_path(agent_dir);
    std::fs::create_dir_all(agent_dir.join("cache"))?;
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(
        &temporary,
        format!("{}\n", serde_json::json!({"terminalTheme":theme})),
    )?;
    std::fs::rename(temporary, path)
}
