//! Port of senpi `packages/coding-agent/src/cli/startup-ui.ts`: the startup themes
//! (`loadThemes` / `loadStartupThemes`), the startup TUI (`createStartupTui` / `startStartupTui` /
//! `queryStartupTerminalColors` / `clearStartupTui`), the first-time setup (`showFirstTimeSetup` /
//! `shouldRunFirstTimeSetup`) and the selector and input dialogs.
use std::path::{Path, PathBuf};

use maho_core::diagnostics::{
    ResourceCollision, ResourceCollisionType, ResourceDiagnostic, ResourceDiagnosticType,
};
use maho_interactive::theme::{
    ColorMode, Theme, detection::detect_terminal_background_from_env, registry::ThemeRegistry,
    resolve_theme_setting,
};

/// senpi `initTheme` outcome: the active theme plus the diagnostics the theme load produced.
pub struct StartupThemeResolution {
    pub theme: Theme,
    pub diagnostics: Vec<String>,
    /// The diagnostic for the configured theme failing to load (also in `diagnostics`); the
    /// interactive mode reports it in the TUI instead (senpi `applyThemeName(themeSetting, true)`).
    pub load_error: Option<String>,
}

/// senpi `loadThemes(paths)` + `dedupeThemes` (resource-loader.ts): a missing path or a non-json
/// file is a warning, a directory loads its `*.json` files, a broken theme is a warning, and the
/// first occurrence of a name wins with later ones reported as collisions.
pub fn load_theme_resources(
    paths: impl IntoIterator<Item = PathBuf>,
    mode: ColorMode,
) -> (Vec<Theme>, Vec<ResourceDiagnostic>) {
    let mut loaded = Vec::new();
    let mut diagnostics = Vec::new();
    for path in paths {
        let resolved = path.display().to_string();
        if !path.exists() {
            diagnostics.push(theme_warning("theme path does not exist".into(), resolved));
            continue;
        }
        match std::fs::metadata(&path) {
            Ok(stats) if stats.is_dir() => {
                load_themes_from_dir(&path, mode, &mut loaded, &mut diagnostics)
            }
            Ok(stats) if stats.is_file() && resolved.ends_with(".json") => {
                load_theme_from_file(&path, mode, &mut loaded, &mut diagnostics)
            }
            Ok(_) => diagnostics.push(theme_warning(
                "theme path is not a json file".into(),
                resolved,
            )),
            Err(error) => diagnostics.push(theme_warning(error.to_string(), resolved)),
        }
    }
    let mut themes: Vec<Theme> = Vec::new();
    for theme in loaded {
        let source = |theme: &Theme| {
            theme
                .source_path
                .as_ref()
                .map_or_else(|| "<builtin>".to_owned(), |path| path.display().to_string())
        };
        if let Some(existing) = themes.iter().find(|existing| existing.name == theme.name) {
            diagnostics.push(ResourceDiagnostic {
                category: None,
                diagnostic_type: ResourceDiagnosticType::Collision,
                message: format!("name \"{}\" collision", theme.name),
                path: theme
                    .source_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                collision: Some(ResourceCollision {
                    resource_type: ResourceCollisionType::Theme,
                    name: theme.name.clone(),
                    winner_path: source(existing),
                    loser_path: source(&theme),
                    winner_source: None,
                    loser_source: None,
                }),
            });
        } else {
            themes.push(theme);
        }
    }
    (themes, diagnostics)
}

fn theme_warning(message: String, path: String) -> ResourceDiagnostic {
    ResourceDiagnostic {
        category: None,
        diagnostic_type: ResourceDiagnosticType::Warning,
        message,
        path: Some(path),
        collision: None,
    }
}

/// senpi `loadThemesFromDir`: every `*.json` file (symlinks followed) in `dir`.
fn load_themes_from_dir(
    dir: &Path,
    mode: ColorMode,
    themes: &mut Vec<Theme>,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            return diagnostics.push(theme_warning(error.to_string(), dir.display().to_string()));
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !entry.file_name().to_string_lossy().ends_with(".json") {
            continue;
        }
        load_theme_from_file(&path, mode, themes, diagnostics);
    }
}

/// senpi `loadThemeFromFile`: a theme that fails to load is a warning on its path.
fn load_theme_from_file(
    path: &Path,
    mode: ColorMode,
    themes: &mut Vec<Theme>,
    diagnostics: &mut Vec<ResourceDiagnostic>,
) {
    match Theme::load_from_path(path, mode) {
        Ok(theme) => themes.push(theme),
        Err(error) => {
            diagnostics.push(theme_warning(error.to_string(), path.display().to_string()))
        }
    }
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
    let mut registry = ThemeRegistry::new(
        custom_directory.to_path_buf(),
        "dark",
        maho_interactive::theme::detect_color_mode(),
    )
    .map_err(|error| error.to_string())?;
    let mut diagnostics = Vec::new();
    if let Err(error) = registry.set_registered_themes(registered) {
        diagnostics.push(error.to_string());
    }
    let mut load_error = None;
    if let Err(error) = registry.set_theme(&name, false) {
        diagnostics.push(error.to_string());
        load_error = Some(error.to_string());
    }
    Ok(StartupThemeResolution {
        theme: registry.current,
        diagnostics,
        load_error,
    })
}

/// senpi `loadThemes(resources)`: every enabled resolved theme resource, first occurrence of a
/// name wins, and a broken theme is skipped silently (the resource loader reports theme
/// diagnostics later in startup).
pub fn load_startup_package_themes(
    resources: &[maho_core::package_manager::ResolvedResource],
    mode: ColorMode,
) -> Vec<Theme> {
    let mut themes: Vec<Theme> = Vec::new();
    for resource in resources.iter().filter(|resource| resource.enabled) {
        let Ok(theme) = Theme::load_from_path(Path::new(&resource.path), mode) else {
            continue;
        };
        if themes.iter().any(|known| known.name == theme.name) {
            continue;
        }
        themes.push(theme);
    }
    themes
}

fn skip_missing_source(
    _source: &str,
) -> futures::future::BoxFuture<'_, maho_core::package_manager::MissingSourceAction> {
    Box::pin(std::future::ready(
        maho_core::package_manager::MissingSourceAction::Skip,
    ))
}

/// senpi `loadStartupThemes(settingsManager)`: the package-resolved themes of the GLOBAL settings
/// only (`SettingsManager.inMemory(getGlobalSettings(), { projectTrusted: false })`), resolved with
/// every missing package skipped, never installed.
pub async fn load_startup_themes(
    global_settings: maho_core::settings_manager::Settings,
    cwd: &str,
    agent_dir: &str,
    mode: ColorMode,
) -> Result<Vec<Theme>, String> {
    let mut settings =
        maho_core::settings_manager::SettingsManager::in_memory(global_settings, false);
    let manager = maho_core::package_manager::DefaultPackageManager::new(
        maho_core::package_manager::PackageManagerOptions {
            cwd,
            agent_dir,
            settings_manager: &mut settings,
        },
    );
    let skip: &maho_core::package_manager::MissingSourceCallback = &skip_missing_source;
    let paths = manager
        .resolve(Some(skip))
        .await
        .map_err(|error| error.to_string())?;
    Ok(load_startup_package_themes(&paths.themes, mode))
}

/// [`load_startup_themes`] for a pre-session dialog of this process. The startup dialogs are sync
/// and may be called on a runtime thread, so the resolve runs on its own thread and runtime. The
/// global settings are read from disk: no session exists yet to hold unsaved overrides. A failed
/// resolve keeps the built-in themes, as upstream's later diagnostics report it.
fn startup_package_themes() -> Vec<Theme> {
    let agent_dir = maho_core::config::get_agent_dir();
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_owned());
    let global = maho_core::settings_manager::SettingsManager::create(
        &cwd,
        &agent_dir,
        &maho_core::config::home_dir(),
        false,
    )
    .get_global()
    .clone();
    let mode = maho_interactive::theme::detect_color_mode();
    let resolved = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime.block_on(load_startup_themes(global, &cwd, &agent_dir, mode))
    })
    .join();
    match resolved {
        Ok(Ok(themes)) => themes,
        Ok(Err(error)) => {
            maho_core::output_guard::maho_write_stderr(&format!(
                "theme: package themes: {error}\n"
            ));
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
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
    use maho_interactive::components::extension_selector::{
        ExtensionSelectorComponent, ExtensionSelectorOptions,
    };
    use std::{cell::RefCell, rc::Rc, sync::Arc};
    let keybindings = Arc::new(
        maho_core::keybindings::KeybindingsManager::create(Some(
            &maho_core::config::get_agent_dir(),
        ))
        .inner()
        .clone(),
    );
    let outcome: Rc<RefCell<Option<Option<String>>>> = Rc::default();
    let selected = outcome.clone();
    let cancelled = outcome.clone();
    run_startup_dialog(
        theme_setting,
        show_hardware_cursor,
        clear_on_shrink,
        maho_interactive::tui_renderer::create_process_terminal(),
        move |theme, rows| {
            Rc::new(RefCell::new(ExtensionSelectorComponent::new(
                theme,
                keybindings,
                title,
                options.to_vec(),
                Box::new(move |option| {
                    *selected.borrow_mut() = Some(Some(option.to_owned()));
                }),
                Box::new(move || {
                    *cancelled.borrow_mut() = Some(None);
                }),
                ExtensionSelectorOptions {
                    terminal_rows: Some(rows),
                    ..Default::default()
                },
            )))
        },
        &outcome,
    )?;
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
    use maho_interactive::components::extension_input::{
        ExtensionInputComponent, ExtensionInputOptions,
    };
    use std::{cell::RefCell, rc::Rc};
    let outcome: Rc<RefCell<Option<Option<String>>>> = Rc::default();
    let submitted = outcome.clone();
    let cancelled = outcome.clone();
    let mounted: Rc<RefCell<Option<Rc<RefCell<ExtensionInputComponent>>>>> = Rc::default();
    let built = mounted.clone();
    let result = run_startup_dialog(
        theme_setting,
        show_hardware_cursor,
        clear_on_shrink,
        maho_interactive::tui_renderer::create_process_terminal(),
        move |theme, _rows| {
            let input = Rc::new(RefCell::new(ExtensionInputComponent::new(
                theme,
                title,
                Box::new(move |value| {
                    *submitted.borrow_mut() = Some(Some(value.to_owned()));
                }),
                Box::new(move || {
                    *cancelled.borrow_mut() = Some(None);
                }),
                ExtensionInputOptions {
                    has_tui: true,
                    ..Default::default()
                },
            )));
            *built.borrow_mut() = Some(input.clone());
            input
        },
        &outcome,
    );
    if let Some(input) = mounted.borrow_mut().take() {
        input.borrow_mut().dispose();
    }
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
    detect_startup_theme_with_registered(theme_setting, screen, terminal, input, Vec::new())
}

/// [`detect_startup_theme`] with the package themes `createStartupTui` registers.
fn detect_startup_theme_with_registered(
    theme_setting: Option<&str>,
    screen: &mut maho_interactive::tui_renderer::InteractiveTui,
    terminal: &mut maho_tui::terminal::ProcessTerminal,
    input: &std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    registered: Vec<Theme>,
) -> Result<Option<Theme>, String> {
    use maho_interactive::theme::{
        TerminalTheme, detection::detect_terminal_theme_for_auto, parse_auto_theme_setting,
    };
    use maho_tui::terminal_colors::TerminalColorScheme;
    if theme_setting.is_some() && parse_auto_theme_setting(theme_setting).is_none() {
        return Ok(None);
    }
    let clock = std::time::Instant::now();
    let now = || u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
    let scheme = screen
        .base_mut()
        .query_terminal_color_scheme(terminal, 100, now());
    let colors = screen
        .base_mut()
        .query_terminal_colors(terminal, 100, now());
    let mut passthrough = Vec::new();
    while scheme.result().is_none() || colors.result().is_none() {
        terminal.pump(16).map_err(|error| error.to_string())?;
        for chunk in std::mem::take(&mut *input.borrow_mut()) {
            if let Some(rest) = screen.base_mut().consume_terminal_protocol_input(&chunk) {
                passthrough.push(rest);
            }
        }
        screen.base_mut().poll_terminal_queries(now());
    }
    let mut pending = passthrough;
    pending.append(&mut input.borrow_mut());
    *input.borrow_mut() = pending;
    if let Some(colors) = colors.result() {
        maho_interactive::theme::registry::set_terminal_colors(&colors);
    }
    let terminal_theme = detect_terminal_theme_for_auto(
        scheme.result().flatten().map(|scheme| match scheme {
            TerminalColorScheme::Light => TerminalTheme::Light,
            TerminalColorScheme::Dark => TerminalTheme::Dark,
        }),
        colors
            .result()
            .and_then(|colors| colors.background)
            .map(|rgb| [rgb.r, rgb.g, rgb.b]),
        std::env::var("COLORFGBG").ok().as_deref(),
    );
    let name = resolve_theme_setting(theme_setting, terminal_theme)
        .unwrap_or_else(|| terminal_theme.name())
        .to_owned();
    resolve_startup_theme_registered(Some(name.as_str()), None, registered).map(Some)
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
    use maho_interactive::tui_renderer::{InteractiveTuiOptions, TuiMode, create_interactive_tui};
    use maho_tui::{terminal::Terminal, tui::Component};
    use std::{cell::RefCell, rc::Rc};
    // senpi `createStartupTui`: `setRegisteredThemes(await loadStartupThemes(settingsManager))`.
    let registered = startup_package_themes();
    let theme = resolve_startup_theme_registered(
        theme_setting,
        std::env::var("COLORFGBG").ok().as_deref(),
        registered.clone(),
    )?;
    let mut screen = create_interactive_tui(
        InteractiveTuiOptions {
            tui_mode: TuiMode::Regular,
            show_hardware_cursor,
            bottom_shortcut: String::new(),
        },
        theme.clone(),
    );
    screen.base_mut().set_clear_on_shrink(clear_on_shrink);
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = input.clone();
    // senpi `startStartupTui` -> `ui.start()`.
    screen.base_mut().prepare_start();
    screen.before_terminal_start(&mut terminal, false, false);
    terminal.start(
        Box::new(move |chunk| captured.borrow_mut().push(chunk.into())),
        Box::new(|| {}),
    );
    screen.base_mut().after_terminal_start(&mut terminal);
    let clock = std::time::Instant::now();
    let result = (|| {
        let theme = detect_startup_theme_with_registered(
            theme_setting,
            &mut screen,
            &mut terminal,
            &input,
            registered,
        )?
        .unwrap_or(theme);
        let dialog = build(&theme, usize::from(terminal.rows()));
        let component: Rc<RefCell<dyn Component>> = dialog.clone();
        screen.base_mut().add_child(component.clone());
        screen.base_mut().set_focus(Some(component));
        while outcome.borrow().is_none() {
            // senpi `TuiBase.handleTerminalInput`: protocol replies are consumed and key releases
            // dropped unless the focused dialog wants them.
            for chunk in std::mem::take(&mut *input.borrow_mut()) {
                screen
                    .base_mut()
                    .handle_terminal_input(&chunk, maho_tui::keys::is_key_release(&chunk));
            }
            let now =
                u64::try_from(clock.elapsed().as_millis()).map_err(|error| error.to_string())?;
            screen.base_mut().request_render(false, now);
            screen.do_render(&mut terminal);
            terminal.pump(16).map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    // senpi `clearStartupTui`: `ui.clear()` then render once so the dialog is erased.
    screen.base_mut().clear();
    screen.base_mut().request_render(
        false,
        u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
    screen.do_render(&mut terminal);
    // senpi `ui.stop()`.
    screen.base_mut().begin_stop(&mut terminal);
    screen.before_terminal_stop(&mut terminal, false);
    screen.base_mut().finish_stop_output(&mut terminal, false);
    let stopped = terminal.stop().map_err(|error| error.to_string());
    screen.after_terminal_stop(&mut terminal, false);
    screen.base_mut().reset_for_stop();
    result.and(stopped)
}

/// senpi `createSystemTheme`: the `system` theme generated from the terminal's reported colors,
/// in grayscale (saturation 0) while they are pending. A token the generator leaves at the terminal
/// default is the empty color. senpi also renders some tokens faint (`dim`); `Theme` has no faint
/// foreground, so those tokens keep their generated color.
pub fn create_system_theme(
    colors: &maho_tui::terminal_colors::TerminalColors,
    pending: bool,
    terminal: maho_interactive::theme::TerminalTheme,
    mode: ColorMode,
) -> Result<Theme, String> {
    maho_interactive::theme::registry::set_terminal_colors(colors);
    use maho_interactive::theme::system_theme::{
        SYSTEM_THEME_NAME, SystemColor, SystemThemeInput, generate_system_theme_colors,
    };
    use maho_interactive::theme::theme_json::{ColorValue, ThemeAppearance, ThemeJson};
    let palette = (colors.palette.len() == 16)
        .then(|| colors.palette.iter().copied().collect::<Option<Vec<_>>>())
        .flatten();
    let generated = generate_system_theme_colors(&SystemThemeInput {
        foreground: colors.foreground,
        background: colors.background,
        palette,
        saturation: Some(if pending { 0.0 } else { 1.0 }),
        appearance_hint: Some(match terminal {
            maho_interactive::theme::TerminalTheme::Dark => ThemeAppearance::Dark,
            maho_interactive::theme::TerminalTheme::Light => ThemeAppearance::Light,
        }),
    });
    let mut values = std::collections::BTreeMap::new();
    for (token, color) in generated.colors {
        let value = match color {
            SystemColor::Hex(hex) => ColorValue::Text(hex),
            SystemColor::Index(index) => {
                ColorValue::Index(u8::try_from(index).map_err(|error| error.to_string())?)
            }
            SystemColor::Default => ColorValue::Text(String::new()),
        };
        values.insert(token, value);
    }
    let json = ThemeJson {
        name: SYSTEM_THEME_NAME.to_owned(),
        appearance: generated.appearance,
        vars: std::collections::BTreeMap::new(),
        colors: values,
        export_colors: std::collections::BTreeMap::new(),
    };
    Theme::from_json(json, mode).map_err(|error| error.to_string())
}

/// The theme state senpi's theme module keeps globally for a startup TUI: the registered package
/// themes, the terminal's reported colors and whether they are still pending.
pub struct StartupThemes {
    registered: Vec<Theme>,
    custom_directory: PathBuf,
    mode: ColorMode,
    colorfgbg: Option<String>,
    colors: maho_tui::terminal_colors::TerminalColors,
    colors_pending: bool,
}
impl StartupThemes {
    /// senpi `getTerminalTheme()`: the reported background, else `COLORFGBG`.
    pub fn terminal_theme(&self) -> maho_interactive::theme::TerminalTheme {
        maho_interactive::theme::theme::detect_terminal_background_theme(
            self.colors.background.map(|rgb| [rgb.r, rgb.g, rgb.b]),
            self.colorfgbg.as_deref(),
        )
        .theme
    }
    pub fn colors_pending(&self) -> bool {
        self.colors_pending
    }
    /// senpi `setTerminalColors`.
    fn set_terminal_colors(&mut self, colors: maho_tui::terminal_colors::TerminalColors) {
        maho_interactive::theme::registry::set_terminal_colors(&colors);
        self.colors = colors;
        self.colors_pending = false;
    }
    /// senpi `loadTheme(name)` with `setTheme`'s fallback: `system` is generated (the name is
    /// reserved), a registered or custom theme is loaded, and a theme that fails to load falls back
    /// to the system theme.
    pub fn load(&self, name: &str) -> Result<Theme, String> {
        use maho_interactive::theme::{registry::ThemeRegistry, system_theme::SYSTEM_THEME_NAME};
        let system = || {
            create_system_theme(
                &self.colors,
                self.colors_pending,
                self.terminal_theme(),
                self.mode,
            )
        };
        if name == SYSTEM_THEME_NAME {
            return system();
        }
        let loaded = ThemeRegistry::new(self.custom_directory.clone(), "dark", self.mode).and_then(
            |mut registry| {
                registry.set_registered_themes(self.registered.clone())?;
                registry.load_theme(name)
            },
        );
        loaded.or_else(|_| system())
    }
    /// senpi `resolveThemeSetting(themeSetting, getTerminalTheme()) ?? SYSTEM_THEME_NAME`, loaded.
    pub fn resolve_setting(&self, setting: Option<&str>) -> Result<Theme, String> {
        let name = resolve_theme_setting(setting, self.terminal_theme())
            .unwrap_or(maho_interactive::theme::system_theme::SYSTEM_THEME_NAME)
            .to_owned();
        self.load(&name)
    }
}

/// What a startup TUI is built from (senpi reads these from the settings manager and globals).
pub struct StartupTuiOptions {
    pub terminal: maho_tui::terminal::ProcessTerminal,
    pub clock: std::sync::Arc<dyn maho_tui::stdin_buffer::Clock>,
    pub theme_setting: Option<String>,
    pub show_hardware_cursor: bool,
    pub clear_on_shrink: bool,
    pub registered_themes: Vec<Theme>,
    pub custom_themes_directory: PathBuf,
    pub color_mode: ColorMode,
    pub colorfgbg: Option<String>,
}

/// Re-resolves the startup theme once the terminal reported its colors (senpi's `onColors`).
type OnColors = Box<dyn FnMut(&StartupThemes) -> Result<Theme, String>>;

/// senpi's startup `TUI`: the screen, its process terminal and the theme state its dialogs use.
pub struct StartupTui {
    pub screen: maho_interactive::tui_renderer::InteractiveTui,
    pub terminal: maho_tui::terminal::ProcessTerminal,
    pub themes: StartupThemes,
    /// The active theme (senpi's global `theme`).
    pub theme: Theme,
    clock: std::sync::Arc<dyn maho_tui::stdin_buffer::Clock>,
    input: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    colors_query:
        Option<maho_tui::tui::TerminalQueryTicket<maho_tui::terminal_colors::TerminalColors>>,
    late_colors: std::rc::Rc<std::cell::RefCell<Option<maho_tui::terminal_colors::TerminalColors>>>,
    on_colors: Option<OnColors>,
}

/// Terminal color query timeout (senpi `TERMINAL_QUERY_TIMEOUT_MS`).
const TERMINAL_QUERY_TIMEOUT_MS: u64 = 100;

impl StartupTui {
    /// The part of senpi `createStartupTui` after its process-global setup:
    /// `markTerminalColorsPending()`, `initTheme(resolveThemeSetting(setting, getTerminalTheme())
    /// ?? SYSTEM_THEME_NAME)` and `new TUI(terminal, showHardwareCursor)` with `setClearOnShrink`.
    pub fn new(options: StartupTuiOptions) -> Result<Self, String> {
        use maho_interactive::tui_renderer::{
            InteractiveTuiOptions, TuiMode, create_interactive_tui,
        };
        let themes = StartupThemes {
            registered: options.registered_themes,
            custom_directory: options.custom_themes_directory,
            mode: options.color_mode,
            colorfgbg: options.colorfgbg,
            colors: maho_tui::terminal_colors::TerminalColors::default(),
            colors_pending: true,
        };
        let theme = themes.resolve_setting(options.theme_setting.as_deref())?;
        let mut screen = create_interactive_tui(
            InteractiveTuiOptions {
                tui_mode: TuiMode::Regular,
                show_hardware_cursor: options.show_hardware_cursor,
                bottom_shortcut: String::new(),
            },
            theme.clone(),
        );
        screen
            .base_mut()
            .set_clear_on_shrink(options.clear_on_shrink);
        Ok(Self {
            screen,
            terminal: options.terminal,
            themes,
            theme,
            clock: options.clock,
            input: std::rc::Rc::default(),
            colors_query: None,
            late_colors: std::rc::Rc::default(),
            on_colors: None,
        })
    }

    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    /// senpi `ui.start()`.
    pub fn start(&mut self) {
        use maho_tui::terminal::Terminal;
        let captured = self.input.clone();
        self.screen.base_mut().prepare_start();
        self.screen
            .before_terminal_start(&mut self.terminal, false, false);
        self.terminal.start(
            Box::new(move |chunk| captured.borrow_mut().push(chunk.into())),
            Box::new(|| {}),
        );
        self.screen
            .base_mut()
            .after_terminal_start(&mut self.terminal);
    }

    /// senpi `setTheme(name)`: load the theme (falling back to the system theme) and make it active.
    pub fn set_theme(&mut self, name: &str) -> Result<&Theme, String> {
        self.theme = self.themes.load(name)?;
        Ok(&self.theme)
    }

    /// senpi `queryStartupTerminalColors(ui, onColors)`: query the terminal's colors without
    /// waiting. When they arrive (also after the timeout, and again on a late reply) [`Self::poll`]
    /// records them for the system theme, installs the theme `on_colors` returns, and re-renders.
    pub fn query_terminal_colors(&mut self, on_colors: OnColors) {
        let late = self.late_colors.clone();
        let now = self.now();
        let ticket = self
            .screen
            .base_mut()
            .query_terminal_colors_with_late_reply(
                &mut self.terminal,
                TERMINAL_QUERY_TIMEOUT_MS,
                now,
                Some(Box::new(
                    move |colors: maho_tui::terminal_colors::TerminalColors| {
                        *late.borrow_mut() = Some(colors)
                    },
                )),
            );
        self.colors_query = Some(ticket);
        self.on_colors = Some(on_colors);
    }

    /// One turn of the startup event loop: read the terminal, deliver input to the focused dialog,
    /// settle due terminal queries, apply arrived terminal colors, then render. True when the
    /// terminal colors changed the active theme.
    pub fn poll(&mut self) -> Result<bool, String> {
        self.terminal.pump(16).map_err(|error| error.to_string())?;
        let now = self.now();
        for chunk in std::mem::take(&mut *self.input.borrow_mut()) {
            self.screen
                .base_mut()
                .handle_terminal_input(&chunk, maho_tui::keys::is_key_release(&chunk));
        }
        self.screen.base_mut().poll_terminal_queries(now);
        let mut changed = false;
        if let Some(colors) = self
            .colors_query
            .as_ref()
            .and_then(maho_tui::tui::TerminalQueryTicket::result)
        {
            self.colors_query = None;
            changed |= self.apply_terminal_colors(colors)?;
        }
        let late = self.late_colors.borrow_mut().take();
        if let Some(colors) = late {
            changed |= self.apply_terminal_colors(colors)?;
        }
        self.screen.base_mut().request_render(false, now);
        self.screen.do_render(&mut self.terminal);
        Ok(changed)
    }

    fn apply_terminal_colors(
        &mut self,
        colors: maho_tui::terminal_colors::TerminalColors,
    ) -> Result<bool, String> {
        self.themes.set_terminal_colors(colors);
        let Some(on_colors) = self.on_colors.as_mut() else {
            return Ok(false);
        };
        self.theme = on_colors(&self.themes)?;
        let now = self.now();
        self.screen.base_mut().invalidate();
        self.screen.base_mut().request_render(false, now);
        Ok(true)
    }

    /// senpi `clearStartupTui`: `ui.clear()` and render once so the dialog is erased. senpi then
    /// waits 25 ms for its throttled render; this render is synchronous, so there is nothing to wait for.
    pub fn clear(&mut self) {
        let now = self.now();
        self.screen.base_mut().clear();
        self.screen.base_mut().request_render(false, now);
        self.screen.do_render(&mut self.terminal);
    }

    /// senpi `ui.stop()`.
    pub fn stop(&mut self) -> Result<(), String> {
        use maho_tui::terminal::Terminal;
        self.screen.base_mut().begin_stop(&mut self.terminal);
        self.screen.before_terminal_stop(&mut self.terminal, false);
        self.screen
            .base_mut()
            .finish_stop_output(&mut self.terminal, false);
        let stopped = self.terminal.stop().map_err(|error| error.to_string());
        self.screen.after_terminal_stop(&mut self.terminal, false);
        self.screen.base_mut().reset_for_stop();
        stopped
    }
}

/// senpi `createStartupTui(settingsManager)`: apply the terminal capability overrides, register the
/// startup package themes and the keybindings, then build the startup TUI on the process terminal.
pub fn create_startup_tui(
    settings: &maho_core::settings_manager::SettingsManager,
) -> Result<StartupTui, String> {
    maho_tui::terminal_image::set_capability_overrides(
        settings.get_terminal_capability_overrides(&maho_core::config::current_env()),
    );
    let registered = startup_package_themes();
    let agent_dir = maho_core::config::get_agent_dir();
    maho_tui::keybindings::set_keybindings(
        maho_core::keybindings::KeybindingsManager::create(Some(&agent_dir))
            .inner()
            .clone(),
    );
    StartupTui::new(StartupTuiOptions {
        terminal: maho_interactive::tui_renderer::create_process_terminal(),
        clock: std::sync::Arc::new(maho_tui::stdin_buffer::SystemClock::default()),
        theme_setting: settings.get_string("theme"),
        show_hardware_cursor: settings.get_show_hardware_cursor(),
        clear_on_shrink: settings.get_clear_on_shrink(),
        registered_themes: registered,
        custom_themes_directory: PathBuf::from(crate::config::get_custom_themes_dir()),
        color_mode: maho_interactive::theme::detect_color_mode(),
        colorfgbg: std::env::var("COLORFGBG").ok(),
    })
}

/// senpi `startStartupTui(ui, settingsManager)`: start the TUI, then query the terminal colors and
/// re-resolve the theme setting against them when they arrive.
pub fn start_startup_tui(ui: &mut StartupTui, theme_setting: Option<String>) {
    ui.start();
    ui.query_terminal_colors(Box::new(move |themes: &StartupThemes| {
        themes.resolve_setting(theme_setting.as_deref())
    }));
}

/// senpi `showFirstTimeSetup(settingsManager)`: the experimental first-time setup dialog; a
/// submitted result persists the theme and the analytics opt-in.
pub fn show_first_time_setup(
    settings: &mut maho_core::settings_manager::SettingsManager,
) -> Result<(), String> {
    let mut ui = create_startup_tui(settings)?;
    run_first_time_setup(&mut ui, settings)
}

/// [`show_first_time_setup`] on a created startup TUI.
pub fn run_first_time_setup(
    ui: &mut StartupTui,
    settings: &mut maho_core::settings_manager::SettingsManager,
) -> Result<(), String> {
    use maho_interactive::components::first_time_setup::{
        FirstTimeSetupComponent, FirstTimeSetupOptions, FirstTimeSetupResult,
    };
    use maho_interactive::theme::system_theme::SYSTEM_THEME_NAME;
    use std::{cell::RefCell, rc::Rc};
    ui.start();
    let preview_theme = Rc::new(RefCell::new(SYSTEM_THEME_NAME.to_owned()));
    let theme = ui.set_theme(SYSTEM_THEME_NAME)?.clone();
    let previewed: Rc<RefCell<Option<String>>> = Rc::default();
    let outcome: Rc<RefCell<Option<Option<FirstTimeSetupResult>>>> = Rc::default();
    let (preview, submitted, cancelled) = (previewed.clone(), outcome.clone(), outcome.clone());
    let component = Rc::new(RefCell::new(FirstTimeSetupComponent::new(
        FirstTimeSetupOptions {
            detected_theme: ui.themes.terminal_theme(),
            theme,
            on_theme_preview: Box::new(move |name: String| *preview.borrow_mut() = Some(name)),
            on_submit: Box::new(move |result: FirstTimeSetupResult| {
                *submitted.borrow_mut() = Some(Some(result))
            }),
            on_cancel: Box::new(move || *cancelled.borrow_mut() = Some(None)),
        },
    )));
    let mounted: Rc<RefCell<dyn maho_tui::tui::Component>> = component.clone();
    ui.screen.base_mut().add_child(mounted.clone());
    ui.screen.base_mut().set_focus(Some(mounted));
    // The terminal's colors regenerate the system theme; re-rendering rebuilds the dialog with it.
    let current_preview = preview_theme.clone();
    ui.query_terminal_colors(Box::new(move |themes: &StartupThemes| {
        themes.load(&current_preview.borrow())
    }));
    let result = (|| {
        while outcome.borrow().is_none() {
            let recolored = ui.poll()?;
            let next = previewed.borrow_mut().take();
            if let Some(name) = next {
                *preview_theme.borrow_mut() = name.clone();
                ui.set_theme(&name)?;
            } else if !recolored {
                continue;
            }
            component.borrow_mut().set_theme(ui.theme.clone());
            ui.screen.base_mut().invalidate();
        }
        Ok::<(), String>(())
    })();
    let chosen = outcome.borrow_mut().take().flatten();
    let persisted = match (&result, chosen) {
        (Ok(()), Some(chosen)) => {
            let values = maho_core::settings_manager::Settings::from_iter([(
                "theme".to_owned(),
                serde_json::Value::String(chosen.theme),
            )]);
            settings
                .set(maho_core::settings_manager::SettingsScope::Global, &values)
                .and_then(|()| settings.set_enable_analytics(chosen.share_analytics))
        }
        _ => Ok(()),
    };
    ui.clear();
    let stopped = ui.stop();
    result.and(persisted).and(stopped)
}

/// Startup entry over the custom themes directory; callers holding resolved package theme
/// resources pass them through [`resolve_startup_theme_with_registered`] instead of losing them.
pub fn resolve_startup_theme(
    setting: Option<&str>,
    colorfgbg: Option<&str>,
) -> Result<Theme, String> {
    resolve_startup_theme_registered(setting, colorfgbg, Vec::new())
}

/// [`resolve_startup_theme`] with registered (package-resolved) themes.
fn resolve_startup_theme_registered(
    setting: Option<&str>,
    colorfgbg: Option<&str>,
    registered: Vec<Theme>,
) -> Result<Theme, String> {
    let custom_directory = PathBuf::from(crate::config::get_custom_themes_dir());
    let resolution =
        resolve_startup_theme_with_registered(setting, colorfgbg, &custom_directory, registered)?;
    for diagnostic in &resolution.diagnostics {
        eprintln!("theme: {diagnostic}");
    }
    Ok(resolution.theme)
}

/// senpi `OFFICIAL_PACKAGE_NAME`/`OFFICIAL_APP_NAME`/`OFFICIAL_CONFIG_DIR_NAME`.
const OFFICIAL_PACKAGE_NAME: &str = "@earendil-works/pi-coding-agent";
const OFFICIAL_APP_NAME: &str = "pi";
const OFFICIAL_CONFIG_DIR_NAME: &str = ".pi";

/// senpi `DistributionMetadata`.
pub struct DistributionMetadata<'a> {
    pub package_name: &'a str,
    pub app_name: &'a str,
    pub config_dir_name: &'a str,
}

fn is_official_distribution(metadata: &DistributionMetadata<'_>) -> bool {
    metadata.package_name == OFFICIAL_PACKAGE_NAME
        && metadata.app_name == OFFICIAL_APP_NAME
        && metadata.config_dir_name == OFFICIAL_CONFIG_DIR_NAME
}

/// senpi `shouldRunFirstTimeSetup` with its inputs explicit: the official Pi distribution, experimental features on,
/// the default agent directory (no `<PREFIX>_CODING_AGENT_DIR`), and no `settings.json` yet.
pub fn should_run_first_time_setup_with(
    metadata: &DistributionMetadata<'_>,
    experimental: bool,
    agent_dir_override: Option<&str>,
    settings_path: &Path,
) -> bool {
    is_official_distribution(metadata)
        && experimental
        && agent_dir_override.is_none_or(str::is_empty)
        && !settings_path.exists()
}

/// senpi `shouldRunFirstTimeSetup()` for this process. mhc and its brands are never the official Pi distribution
/// (there is no npm `PACKAGE_NAME`; the app is the brand name), so this is always false and the first-time setup dialog
/// (`showFirstTimeSetup`) is never shown, exactly as upstream behaves for any fork or rebrand.
pub fn should_run_first_time_setup() -> bool {
    let app_name = maho_core::config::app_name();
    let config_dir_name = maho_core::config::config_dir_name();
    let agent_dir_override = std::env::var(maho_core::config::env_agent_dir_var()).ok();
    should_run_first_time_setup_with(
        &DistributionMetadata {
            package_name: "",
            app_name: &app_name,
            config_dir_name: &config_dir_name,
        },
        maho_core::experimental::are_experimental_features_enabled(),
        agent_dir_override.as_deref(),
        Path::new(&maho_core::config::get_settings_path()),
    )
}

#[cfg(test)]
mod startup_theme_tests {
    use super::*;
    use maho_core::package_manager::{PathMetadata, ResolvedResource};
    use maho_core::source_info::SourceScope;

    fn theme_file(dir: &Path, file: &str, name: &str) -> String {
        let source = include_str!("../../../maho-interactive/src/theme/dark.json");
        let mut json: serde_json::Value = serde_json::from_str(source).expect("dark theme json");
        json["name"] = serde_json::Value::String(name.to_owned());
        let path = dir.join(file);
        std::fs::write(&path, json.to_string()).expect("theme file");
        path.to_string_lossy().into_owned()
    }

    fn resource(path: String, enabled: bool) -> ResolvedResource {
        ResolvedResource {
            path,
            enabled,
            metadata: PathMetadata {
                skill_provenance: None,
                source: "npm:themes".into(),
                scope: SourceScope::User,
                origin: maho_core::source_info::SourceOrigin::Package,
                base_dir: None,
            },
        }
    }

    #[test]
    fn package_themes_keep_enabled_first_named_themes_and_skip_broken_ones() {
        let dir = tempfile::tempdir().expect("temp dir");
        let broken = dir.path().join("broken.json");
        std::fs::write(&broken, "{ not json").expect("broken theme");
        let resources = vec![
            resource(theme_file(dir.path(), "off.json", "disabled-theme"), false),
            resource(broken.to_string_lossy().into_owned(), true),
            resource(theme_file(dir.path(), "a.json", "pkg-theme"), true),
            resource(theme_file(dir.path(), "b.json", "pkg-theme"), true),
            resource(theme_file(dir.path(), "c.json", "other-theme"), true),
        ];
        let themes = load_startup_package_themes(&resources, ColorMode::Truecolor);
        let names: Vec<_> = themes.iter().map(|theme| theme.name.as_str()).collect();
        assert_eq!(names, ["pkg-theme", "other-theme"]);
        assert_eq!(
            themes[0].source_path.as_deref(),
            Some(dir.path().join("a.json").as_path())
        );
    }

    #[test]
    fn a_registered_package_theme_is_the_startup_theme_it_names() {
        let dir = tempfile::tempdir().expect("temp dir");
        let themes = load_startup_package_themes(
            &[resource(
                theme_file(dir.path(), "a.json", "pkg-theme"),
                true,
            )],
            ColorMode::Truecolor,
        );
        let resolution = resolve_startup_theme_with_registered(
            Some("pkg-theme"),
            Some("15;0"),
            dir.path(),
            themes,
        )
        .expect("resolve");
        assert_eq!(resolution.theme.name, "pkg-theme");
        assert!(
            resolution.load_error.is_none(),
            "{:?}",
            resolution.diagnostics
        );
    }

    #[tokio::test]
    async fn startup_themes_come_from_global_packages_only() {
        let agent = tempfile::tempdir().expect("agent dir");
        let cwd = tempfile::tempdir().expect("cwd");
        let package = agent.path().join("my-themes");
        std::fs::create_dir_all(package.join("themes")).expect("package themes dir");
        theme_file(&package.join("themes"), "global.json", "global-theme");
        std::fs::write(
            package.join("package.json"),
            r#"{"name":"my-themes","pi":{"themes":["themes"]}}"#,
        )
        .expect("package.json");
        let mut global = maho_core::settings_manager::Settings::new();
        global.insert(
            "packages".into(),
            serde_json::json!([package.to_string_lossy()]),
        );
        let themes = load_startup_themes(
            global,
            &cwd.path().to_string_lossy(),
            &agent.path().to_string_lossy(),
            ColorMode::Truecolor,
        )
        .await
        .expect("resolve");
        let names: Vec<_> = themes.iter().map(|theme| theme.name.as_str()).collect();
        assert_eq!(names, ["global-theme"]);
    }
}

#[cfg(test)]
mod first_time_setup_tests {
    use super::*;

    const OFFICIAL: DistributionMetadata<'static> = DistributionMetadata {
        package_name: OFFICIAL_PACKAGE_NAME,
        app_name: OFFICIAL_APP_NAME,
        config_dir_name: OFFICIAL_CONFIG_DIR_NAME,
    };

    fn settings() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.json");
        (dir, path)
    }

    #[test]
    fn returns_true_when_experimental_default_agent_dir_and_no_settings_json() {
        let (_dir, path) = settings();
        assert!(should_run_first_time_setup_with(
            &OFFICIAL, true, None, &path
        ));
    }

    #[test]
    fn returns_false_when_experimental_features_are_disabled() {
        let (_dir, path) = settings();
        assert!(!should_run_first_time_setup_with(
            &OFFICIAL, false, None, &path
        ));
    }

    #[test]
    fn returns_false_when_a_custom_agent_dir_is_set() {
        let (dir, path) = settings();
        assert!(!should_run_first_time_setup_with(
            &OFFICIAL,
            true,
            Some(&dir.path().to_string_lossy()),
            &path
        ));
    }

    #[test]
    fn returns_false_when_settings_json_already_exists() {
        let (_dir, path) = settings();
        std::fs::write(&path, "{}").expect("settings");
        assert!(!should_run_first_time_setup_with(
            &OFFICIAL, true, None, &path
        ));
    }

    #[test]
    fn a_rebranded_distribution_never_runs_first_time_setup() {
        let (_dir, path) = settings();
        let maho = DistributionMetadata {
            package_name: "",
            app_name: "maho",
            config_dir_name: ".omo",
        };
        assert!(!should_run_first_time_setup_with(&maho, true, None, &path));
    }
}

#[cfg(test)]
mod startup_tui_tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// A clock that never advances: no terminal timer fires, so only scripted replies settle queries.
    struct FixedClock;
    impl maho_tui::stdin_buffer::Clock for FixedClock {
        fn now_ms(&self) -> u64 {
            0
        }
    }

    /// Scripted stdin: one queued chunk per read, an error once the script is exhausted (so a
    /// dialog that never settles fails instead of waiting), and every write recorded.
    struct ScriptedIo {
        input: VecDeque<String>,
        writes: Arc<Mutex<String>>,
    }
    impl maho_tui::terminal::TerminalIo for ScriptedIo {
        fn write(&mut self, data: &str) {
            self.writes.lock().expect("writes").push_str(data);
        }
        fn is_raw(&self) -> bool {
            false
        }
        fn set_raw_mode(&mut self, _: bool) -> Result<(), maho_tui::terminal::TerminalError> {
            Ok(())
        }
        fn size(&self) -> Option<(u16, u16)> {
            Some((80, 24))
        }
        fn read_input(
            &mut self,
            _: u64,
        ) -> Result<Option<Vec<u8>>, maho_tui::terminal::TerminalError> {
            match self.input.pop_front() {
                Some(chunk) => Ok(Some(chunk.into_bytes())),
                None => Err(maho_tui::terminal::TerminalError::new(
                    "scripted input exhausted",
                )),
            }
        }
        fn refresh_dimensions(&mut self) {}
    }

    const BLACK_BACKGROUND: &str = "\x1b]11;rgb:0000/0000/0000\x07";
    const WHITE_FOREGROUND: &str = "\x1b]10;rgb:ffff/ffff/ffff\x07";
    const DA1: &str = "\x1b[?62;c";

    fn startup_tui(
        theme_setting: Option<&str>,
        input: &[&str],
    ) -> (StartupTui, Arc<Mutex<String>>, tempfile::TempDir) {
        let writes = Arc::new(Mutex::new(String::new()));
        let terminal =
            maho_tui::terminal::ProcessTerminal::new(maho_tui::terminal::ProcessTerminalOptions {
                io: Some(Box::new(ScriptedIo {
                    input: input.iter().map(|chunk| (*chunk).to_owned()).collect(),
                    writes: writes.clone(),
                })),
                clock: Some(Arc::new(FixedClock)),
                ..Default::default()
            });
        let themes = tempfile::tempdir().expect("themes dir");
        let ui = StartupTui::new(StartupTuiOptions {
            terminal,
            clock: Arc::new(FixedClock),
            theme_setting: theme_setting.map(str::to_owned),
            show_hardware_cursor: false,
            clear_on_shrink: false,
            registered_themes: Vec::new(),
            custom_themes_directory: themes.path().to_path_buf(),
            color_mode: ColorMode::Truecolor,
            colorfgbg: None,
        })
        .expect("startup tui");
        (ui, writes, themes)
    }

    /// No kitty negotiation and no burst-paste hold, so each scripted key is delivered as itself.
    fn scripted_env<R>(run: impl FnOnce() -> R) -> R {
        maho_tui::process_env::with_overrides(
            &[
                ("PI_TUI_KEYBOARD_PROTOCOL", Some("0")),
                ("PI_TUI_BURST_WINDOW_MS", Some("0")),
                ("TMUX", None),
                ("TMUX_PANE", None),
                ("TERM_PROGRAM", None),
            ],
            run,
        )
    }

    #[test]
    fn create_startup_tui_starts_on_the_pending_grayscale_system_theme() {
        let (ui, _, _dir) = startup_tui(None, &[]);
        assert_eq!(
            ui.theme.name,
            maho_interactive::theme::system_theme::SYSTEM_THEME_NAME
        );
        assert!(ui.themes.colors_pending());
        let grayscale = create_system_theme(
            &Default::default(),
            true,
            maho_interactive::theme::TerminalTheme::Dark,
            ColorMode::Truecolor,
        )
        .expect("grayscale");
        assert_eq!(
            ui.theme
                .fg(maho_interactive::theme::ThemeColor::Accent, "x"),
            grayscale.fg(maho_interactive::theme::ThemeColor::Accent, "x")
        );
    }

    #[test]
    fn an_explicit_theme_setting_is_the_startup_theme() {
        let (ui, _, _dir) = startup_tui(Some("light"), &[]);
        assert_eq!(ui.theme.name, "light");
    }

    #[test]
    fn query_startup_terminal_colors_records_the_reply_and_reresolves_the_theme() {
        scripted_env(|| {
            let (mut ui, writes, _dir) =
                startup_tui(None, &[WHITE_FOREGROUND, BLACK_BACKGROUND, DA1]);
            start_startup_tui(&mut ui, None);
            assert!(
                writes.lock().expect("writes").contains("\x1b]11;?\x07"),
                "the startup TUI queries the terminal colors"
            );
            let mut recolored = false;
            for _ in 0..3 {
                recolored |= ui.poll().expect("poll");
            }
            assert!(recolored);
            assert!(!ui.themes.colors_pending());
            assert_eq!(
                ui.themes.terminal_theme(),
                maho_interactive::theme::TerminalTheme::Dark
            );
            let reported = maho_tui::terminal_colors::TerminalColors {
                foreground: Some(maho_tui::terminal_colors::RgbColor {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
                background: Some(maho_tui::terminal_colors::RgbColor { r: 0, g: 0, b: 0 }),
                palette: Vec::new(),
            };
            let expected = create_system_theme(
                &reported,
                false,
                maho_interactive::theme::TerminalTheme::Dark,
                ColorMode::Truecolor,
            )
            .expect("system theme");
            assert_eq!(
                ui.theme.fg(maho_interactive::theme::ThemeColor::Text, "x"),
                expected.fg(maho_interactive::theme::ThemeColor::Text, "x")
            );
            ui.clear();
            ui.stop().expect("stop");
        });
    }

    #[test]
    fn first_time_setup_persists_the_chosen_theme_and_the_analytics_opt_in() {
        scripted_env(|| {
            // Move the theme to "dark", continue, then keep "Share anonymous usage data" and finish.
            let (mut ui, _, _dir) = startup_tui(None, &[BLACK_BACKGROUND, DA1, "j", "\r", "\r"]);
            let mut settings =
                maho_core::settings_manager::SettingsManager::in_memory(Default::default(), false);
            run_first_time_setup(&mut ui, &mut settings).expect("first-time setup");
            assert_eq!(settings.get_string("theme").as_deref(), Some("dark"));
            assert!(settings.get_enable_analytics());
            assert!(settings.get_tracking_id().is_some());
            assert_eq!(
                ui.theme.name, "dark",
                "the theme preview is applied while the dialog runs"
            );
        });
    }

    #[test]
    fn first_time_setup_can_opt_out_of_analytics() {
        scripted_env(|| {
            let (mut ui, _, _dir) = startup_tui(None, &["\r", "j", "\r"]);
            let mut settings =
                maho_core::settings_manager::SettingsManager::in_memory(Default::default(), false);
            run_first_time_setup(&mut ui, &mut settings).expect("first-time setup");
            assert_eq!(settings.get_string("theme").as_deref(), Some("system"));
            assert!(!settings.get_enable_analytics());
            assert_eq!(settings.get_tracking_id(), None);
        });
    }

    #[test]
    fn a_cancelled_first_time_setup_persists_nothing() {
        scripted_env(|| {
            let (mut ui, _, _dir) = startup_tui(None, &["j", "\x03"]);
            let mut settings =
                maho_core::settings_manager::SettingsManager::in_memory(Default::default(), false);
            run_first_time_setup(&mut ui, &mut settings).expect("first-time setup");
            assert_eq!(settings.get_string("theme"), None);
            assert!(!settings.get_enable_analytics());
        });
    }
}
