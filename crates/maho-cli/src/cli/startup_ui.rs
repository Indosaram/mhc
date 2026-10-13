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
    use maho_tui::terminal::ProcessTerminal;
    let keybindings = Arc::new(maho_core::keybindings::KeybindingsManager::create(Some(&maho_core::config::get_agent_dir())).inner().clone());
    let outcome: Rc<RefCell<Option<Option<String>>>> = Rc::default();
    let selected = outcome.clone();
    let cancelled = outcome.clone();
    run_startup_dialog(theme_setting, show_hardware_cursor, clear_on_shrink, ProcessTerminal::default(), move |theme, rows| {
        Rc::new(RefCell::new(ExtensionSelectorComponent::new(
            theme, keybindings, title, options.to_vec(),
            Box::new(move |option| { *selected.borrow_mut() = Some(Some(option.to_owned())); }),
            Box::new(move || { *cancelled.borrow_mut() = Some(None); }),
            ExtensionSelectorOptions { terminal_rows: Some(rows), ..Default::default() },
        )))
    }, &outcome)?;
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
    let outcome: Rc<RefCell<Option<Option<String>>>> = Rc::default();
    let submitted = outcome.clone();
    let cancelled = outcome.clone();
    let mounted: Rc<RefCell<Option<Rc<RefCell<ExtensionInputComponent>>>>> = Rc::default();
    let built = mounted.clone();
    let result = run_startup_dialog(theme_setting, show_hardware_cursor, clear_on_shrink, ProcessTerminal::default(), move |theme, _rows| {
        let input = Rc::new(RefCell::new(ExtensionInputComponent::new(
            theme, title,
            Box::new(move |value| { *submitted.borrow_mut() = Some(Some(value.to_owned())); }),
            Box::new(move || { *cancelled.borrow_mut() = Some(None); }),
            ExtensionInputOptions { has_tui: true, ..Default::default() },
        )));
        *built.borrow_mut() = Some(input.clone());
        input
    }, &outcome);
    if let Some(input) = mounted.borrow_mut().take() { input.borrow_mut().dispose(); }
    result?;
    let value = outcome.borrow_mut().take().flatten();
    Ok(value)
}

/// senpi `applyDetectedStartupTheme`: an unset or `light/dark` theme setting queries the started
/// terminal (`detectTerminalThemeForAuto`: DSR 996 color scheme, else OSC 11 background, else
/// `COLORFGBG`, 100ms timeout) and resolves the startup theme from the answer. Any other setting
/// keeps the environment-resolved theme (`None`). Non-protocol input that arrives while waiting
/// is left in `input` for the dialog.
pub(crate) fn detect_startup_theme(
    theme_setting: Option<&str>,
    screen: &mut maho_interactive::tui_renderer::InteractiveTui,
    terminal: &mut maho_tui::terminal::ProcessTerminal,
    input: &std::rc::Rc<std::cell::RefCell<Vec<String>>>,
) -> Result<Option<Theme>, String> {
    use maho_interactive::theme::{TerminalTheme, detection::detect_terminal_theme_for_auto, parse_auto_theme_setting};
    use maho_tui::terminal_colors::TerminalColorScheme;
    if theme_setting.is_some() && parse_auto_theme_setting(theme_setting).is_none() {
        return Ok(None);
    }
    let clock = std::time::Instant::now();
    let now = || u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
    let scheme = screen.base_mut().query_terminal_color_scheme(terminal, 100, now());
    let background = screen.base_mut().query_terminal_background_color(terminal, 100, now());
    let mut passthrough = Vec::new();
    while scheme.result().is_none() || background.result().is_none() {
        terminal.pump(16).map_err(|error| error.to_string())?;
        for chunk in std::mem::take(&mut *input.borrow_mut()) {
            if let Some(rest) = screen.base_mut().consume_terminal_protocol_input(&chunk) { passthrough.push(rest); }
        }
        screen.base_mut().poll_terminal_queries(now());
    }
    let mut pending = passthrough;
    pending.append(&mut input.borrow_mut());
    *input.borrow_mut() = pending;
    let terminal_theme = detect_terminal_theme_for_auto(
        scheme.result().flatten().map(|scheme| match scheme {
            TerminalColorScheme::Light => TerminalTheme::Light,
            TerminalColorScheme::Dark => TerminalTheme::Dark,
        }),
        background.result().flatten().map(|rgb| [rgb.r, rgb.g, rgb.b]),
        std::env::var("COLORFGBG").ok().as_deref(),
    );
    let name = resolve_theme_setting(theme_setting, terminal_theme).unwrap_or_else(|| terminal_theme.name()).to_owned();
    resolve_startup_theme(Some(name.as_str()), None).map(Some)
}

/// The shared startup-TUI lifecycle of senpi `showStartupSelector`/`showStartupInput`
/// (`createStartupTui` + `startStartupTui`): start the terminal, apply the detected startup theme,
/// mount and focus the dialog `build` creates for that theme and the terminal rows, feed it input
/// until it settles `outcome`, then stop the terminal.
fn run_startup_dialog<C: maho_tui::tui::Component + 'static, T>(
    theme_setting: Option<&str>,
    show_hardware_cursor: bool,
    clear_on_shrink: bool,
    mut terminal: maho_tui::terminal::ProcessTerminal,
    build: impl FnOnce(&Theme, usize) -> std::rc::Rc<std::cell::RefCell<C>>,
    outcome: &std::rc::Rc<std::cell::RefCell<Option<T>>>,
) -> Result<(), String> {
    use std::{cell::RefCell, rc::Rc};
    use maho_interactive::tui_renderer::{create_interactive_tui, InteractiveTuiOptions, TuiMode};
    use maho_tui::{terminal::Terminal, tui::Component};
    let theme = resolve_startup_theme(theme_setting, std::env::var("COLORFGBG").ok().as_deref())?;
    let mut screen = create_interactive_tui(InteractiveTuiOptions {
        tui_mode: TuiMode::Regular, show_hardware_cursor, bottom_shortcut: String::new(),
    }, theme.clone());
    screen.base_mut().set_clear_on_shrink(clear_on_shrink);
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = input.clone();
    screen.before_terminal_start(&mut terminal, false, false);
    terminal.start(Box::new(move |chunk| captured.borrow_mut().push(chunk.into())), Box::new(|| {}));
    let clock = std::time::Instant::now();
    let result = (|| {
        let theme = detect_startup_theme(theme_setting, &mut screen, &mut terminal, &input)?.unwrap_or(theme);
        let dialog = build(&theme, usize::from(terminal.rows()));
        let component: Rc<RefCell<dyn Component>> = dialog.clone();
        screen.base_mut().add_child(component.clone());
        screen.base_mut().set_focus(Some(component));
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
