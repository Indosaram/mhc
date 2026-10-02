use maho_interactive::theme::{ColorMode, Theme, theme::{TerminalTheme, resolve_theme_setting}};

pub fn resolve_startup_theme(setting: Option<&str>, colorfgbg: Option<&str>) -> Result<Theme, String> {
    let terminal = maho_interactive::theme::detection::detect_terminal_background_from_env(colorfgbg).theme;
    let fallback = match terminal { TerminalTheme::Light => "light", TerminalTheme::Dark => "dark" };
    Theme::builtin(resolve_theme_setting(setting, terminal).unwrap_or(fallback), ColorMode::Truecolor)
        .map_err(|error| error.to_string())
}
