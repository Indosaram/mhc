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
    let mut registry = ThemeRegistry::new(custom_directory.to_path_buf(), "dark", maho_interactive::theme::detect_color_mode())
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

/// senpi `showStartupSelector`: a one-shot startup TUI holding an extension selector; resolves the
/// chosen label, or `None` when cancelled.
pub fn show_startup_selector(
    theme_setting: Option<&str>,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    title: &str,
    options: &[String],
) -> Result<Option<String>, String> {
    use std::{cell::RefCell, rc::Rc, sync::Arc};
    use maho_interactive::components::extension_selector::{ExtensionSelectorComponent, ExtensionSelectorOptions};
    use maho_tui::terminal::{ProcessTerminal, Terminal};
    let theme = resolve_startup_theme(theme_setting, std::env::var("COLORFGBG").ok().as_deref())?;
    let keybindings = Arc::new(maho_core::keybindings::KeybindingsManager::create(Some(&maho_core::config::get_agent_dir())).inner().clone());
    let outcome: Rc<RefCell<Option<Option<String>>>> = Rc::default();
    let selected = outcome.clone();
    let cancelled = outcome.clone();
    let mut terminal = ProcessTerminal::default();
    let selector = Rc::new(RefCell::new(ExtensionSelectorComponent::new(
        &theme, keybindings, title, options.to_vec(),
        Box::new(move |option| { *selected.borrow_mut() = Some(Some(option.to_owned())); }),
        Box::new(move || { *cancelled.borrow_mut() = Some(None); }),
        ExtensionSelectorOptions { terminal_rows: Some(usize::from(terminal.rows())), ..Default::default() },
    )));
    run_startup_dialog(theme, show_hardware_cursor, clear_on_shrink, terminal, selector, &outcome)?;
    let selected = outcome.borrow_mut().take().flatten();
    Ok(selected)
}

/// senpi `showStartupInput`: a one-shot startup TUI holding an extension input; resolves the
/// submitted text, or `None` when cancelled. senpi's `ExtensionInputComponent` ignores the
/// placeholder, so it is accepted here only for signature parity.
pub fn show_startup_input(
    theme_setting: Option<&str>,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    title: &str,
    _placeholder: Option<&str>,
) -> Result<Option<String>, String> {
    use std::{cell::RefCell, rc::Rc};
    use maho_interactive::components::extension_input::{ExtensionInputComponent, ExtensionInputOptions};
    use maho_tui::terminal::ProcessTerminal;
    let theme = resolve_startup_theme(theme_setting, std::env::var("COLORFGBG").ok().as_deref())?;
    let outcome: Rc<RefCell<Option<Option<String>>>> = Rc::default();
    let submitted = outcome.clone();
    let cancelled = outcome.clone();
    let input = Rc::new(RefCell::new(ExtensionInputComponent::new(
        &theme, title,
        Box::new(move |value| { *submitted.borrow_mut() = Some(Some(value.to_owned())); }),
        Box::new(move || { *cancelled.borrow_mut() = Some(None); }),
        ExtensionInputOptions { has_tui: true, ..Default::default() },
    )));
    let result = run_startup_dialog(theme, show_hardware_cursor, clear_on_shrink, ProcessTerminal::default(), input.clone(), &outcome);
    input.borrow_mut().dispose();
    result?;
    let value = outcome.borrow_mut().take().flatten();
    Ok(value)
}

/// The shared startup-TUI lifecycle of senpi `showStartupSelector`/`showStartupInput`: mount and
/// focus the dialog, feed it input until it settles `outcome`, then stop the terminal.
fn run_startup_dialog<C: maho_tui::tui::Component + 'static, T>(
    theme: Theme,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    mut terminal: maho_tui::terminal::ProcessTerminal,
    dialog: std::rc::Rc<std::cell::RefCell<C>>,
    outcome: &std::rc::Rc<std::cell::RefCell<Option<T>>>,
) -> Result<(), String> {
    use std::{cell::RefCell, rc::Rc};
    use maho_interactive::tui_renderer::{create_interactive_tui, InteractiveTuiOptions, TuiMode};
    use maho_tui::{terminal::Terminal, tui::Component};
    let component: Rc<RefCell<dyn Component>> = dialog.clone();
    let mut screen = create_interactive_tui(InteractiveTuiOptions {
        tui_mode: TuiMode::Regular, show_hardware_cursor, bottom_shortcut: String::new(),
    }, theme);
    screen.base_mut().set_clear_on_shrink(clear_on_shrink);
    screen.base_mut().add_child(component.clone());
    screen.base_mut().set_focus(Some(component));
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = input.clone();
    screen.before_terminal_start(&mut terminal, false, false);
    terminal.start(Box::new(move |chunk| captured.borrow_mut().push(chunk.into())), Box::new(|| {}));
    let clock = std::time::Instant::now();
    let result = (|| {
        while outcome.borrow().is_none() {
            for chunk in std::mem::take(&mut *input.borrow_mut()) { dialog.borrow_mut().handle_input(&chunk); }
            let now = u64::try_from(clock.elapsed().as_millis()).map_err(|error| error.to_string())?;
            screen.base_mut().request_render(false, now);
            screen.do_render(&mut terminal);
            terminal.pump(16).map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    screen.before_terminal_stop(&mut terminal, false);
    let stopped = terminal.stop().map_err(|error| error.to_string());
    screen.after_terminal_stop(&mut terminal, false);
    result.and(stopped)
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
