//! Port of the startup-theme half of senpi `packages/coding-agent/src/cli/startup-ui.ts`
//! (`loadThemes` / `loadStartupThemes` / `createStartupTui`) over the existing theme registry.
use std::path::{Path, PathBuf};

use maho_interactive::theme::{
    ColorMode, Theme, detection::detect_terminal_background_from_env, registry::ThemeRegistry,
    resolve_theme_setting,
};

/// senpi `initTheme` outcome: the active theme plus the diagnostics the theme load produced.
pub struct StartupThemeResolution {
    pub theme: Theme,
    pub diagnostics: Vec<String>,
}

/// senpi `loadThemes(resources)`: first occurrence of a name wins, a broken theme is reported.
pub fn load_theme_resources(
    paths: impl IntoIterator<Item = PathBuf>,
    mode: ColorMode,
) -> (Vec<Theme>, Vec<String>) {
    let mut themes = Vec::new();
    let mut diagnostics = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for path in paths {
        match Theme::load_from_path(&path, mode) {
            Ok(theme) if seen.insert(theme.name.clone()) => themes.push(theme),
            Ok(_) => {}
            Err(error) => diagnostics.push(format!("{}: {error}", path.display())),
        }
    }
    (themes, diagnostics)
}

/// senpi `createStartupTui`: `setRegisteredThemes(loadStartupThemes())` then
/// `initTheme(resolveThemeSetting(setting, terminalTheme) ?? terminalTheme)`, where the registry
/// resolves the name and keeps the pinned `dark` fallback for a missing/invalid theme.
pub fn resolve_startup_theme_with_registered(
    setting: Option<&str>,
    colorfgbg: Option<&str>,
    custom_directory: &Path,
    registered: Vec<Theme>,
) -> Result<StartupThemeResolution, String> {
    let terminal = detect_terminal_background_from_env(colorfgbg).theme;
    let name = resolve_theme_setting(setting, terminal)
        .unwrap_or_else(|| terminal.name())
        .to_owned();
    let mut registry = ThemeRegistry::new(custom_directory.to_path_buf(), "dark", ColorMode::Truecolor)
        .map_err(|error| error.to_string())?;
    let mut diagnostics = Vec::new();
    if let Err(error) = registry.set_registered_themes(registered) {
        diagnostics.push(error.to_string());
    }
    if let Err(error) = registry.set_theme(&name, false) {
        diagnostics.push(error.to_string());
    }
    Ok(StartupThemeResolution { theme: registry.current, diagnostics })
}

/// Startup entry over the custom themes directory; callers holding resolved package theme
/// resources pass them through [`resolve_startup_theme_with_registered`] instead of losing them.
pub fn resolve_startup_theme(setting: Option<&str>, colorfgbg: Option<&str>) -> Result<Theme, String> {
    let custom_directory = PathBuf::from(crate::config::get_custom_themes_dir());
    let resolution =
        resolve_startup_theme_with_registered(setting, colorfgbg, &custom_directory, Vec::new())?;
    for diagnostic in &resolution.diagnostics {
        eprintln!("theme: {diagnostic}");
    }
    Ok(resolution.theme)
}
