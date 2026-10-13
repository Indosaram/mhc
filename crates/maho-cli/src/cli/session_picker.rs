//! Port of senpi `packages/coding-agent/src/cli/session-picker.ts`: the TUI session selector shown
//! for `--resume` before any session exists.
use std::{cell::RefCell, rc::Rc, sync::Arc};

use maho_core::session_manager::SessionManager;
use maho_interactive::components::{
    session_selector::{SessionSelectorComponent, SessionsLoader},
    session_selector_search::SessionInfo,
};

fn to_picker_row(info: maho_core::session_discovery::SessionInfo) -> SessionInfo {
    SessionInfo {
        path: info.path, id: info.id, cwd: info.cwd, name: info.name,
        parent_session_path: info.parent_session_path, modified_ms: info.modified.timestamp_millis(),
        message_count: info.message_count, first_message: info.first_message, all_messages_text: info.all_messages_text,
    }
}

/// senpi `selectSession(SessionManager.list(cwd, sessionDir), SessionManager.listAll(sessionDir),
/// settingsManager)`: the selected session path, or `None` when cancelled. The selector's exit
/// action stops the TUI and exits the process with code 0, as upstream does.
pub fn select_session(
    cwd: &str,
    session_dir: Option<&str>,
    settings: &maho_core::settings_manager::SettingsManager,
) -> Result<Option<String>, String> {
    use maho_interactive::tui_renderer::{create_interactive_tui, InteractiveTuiOptions, TuiMode};
    use maho_tui::{terminal::{ProcessTerminal, Terminal}, tui::Component};
    let theme = super::startup_ui::resolve_startup_theme(settings.get_string("theme").as_deref(), std::env::var("COLORFGBG").ok().as_deref())?;
    let keybindings = Arc::new(maho_core::keybindings::KeybindingsManager::create(Some(&maho_core::config::get_agent_dir())).inner().clone());
    let (current_cwd, current_dir, all_dir) = (cwd.to_owned(), session_dir.map(str::to_owned), session_dir.map(str::to_owned));
    let current_loader: SessionsLoader = Box::new(move |_| {
        SessionManager::list(&current_cwd, current_dir.as_deref()).into_iter().map(to_picker_row).collect()
    });
    let all_loader: SessionsLoader = Box::new(move |_| {
        SessionManager::list_all(all_dir.as_deref()).into_iter().map(to_picker_row).collect()
    });
    let outcome: Rc<RefCell<Option<Option<String>>>> = Rc::default();
    let exited = Rc::new(RefCell::new(false));
    let (selected, cancelled, exit) = (outcome.clone(), outcome.clone(), exited.clone());
    let mut terminal = ProcessTerminal::default();
    let mut screen = create_interactive_tui(InteractiveTuiOptions {
        tui_mode: TuiMode::Regular,
        show_hardware_cursor: settings.get_bool("showHardwareCursor").unwrap_or(false),
        bottom_shortcut: String::new(),
    }, theme.clone());
    screen.base_mut().set_clear_on_shrink(settings.get_bool("clearOnShrink").unwrap_or(false));
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = input.clone();
    screen.before_terminal_start(&mut terminal, false, false);
    terminal.start(Box::new(move |chunk| captured.borrow_mut().push(chunk.into())), Box::new(|| {}));
    let clock = std::time::Instant::now();
    let result = (|| {
        // senpi `startStartupTui`: `applyDetectedStartupTheme` once the terminal is up.
        let theme = super::startup_ui::detect_startup_theme(settings.get_string("theme").as_deref(), &mut screen, &mut terminal, &input)?.unwrap_or(theme);
        let selector = Rc::new(RefCell::new(SessionSelectorComponent::new(
            &theme, keybindings, current_loader, all_loader,
            Box::new(move |path| { if selected.borrow().is_none() { *selected.borrow_mut() = Some(Some(path.to_owned())); } }),
            Box::new(move || { if cancelled.borrow().is_none() { *cancelled.borrow_mut() = Some(None); } }),
            Box::new(move || { *exit.borrow_mut() = true; }),
            Box::new(|| {}),
            None, Some(false), None, std::env::var("HOME").ok(),
        )));
        let component: Rc<RefCell<dyn Component>> = selector.clone();
        screen.base_mut().add_child(component.clone());
        screen.base_mut().set_focus(Some(component));
        while outcome.borrow().is_none() && !*exited.borrow() {
            for chunk in std::mem::take(&mut *input.borrow_mut()) { selector.borrow_mut().handle_input(&chunk); }
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
    result.and(stopped)?;
    if *exited.borrow() { std::process::exit(0); }
    let selected = outcome.borrow_mut().take().flatten();
    Ok(selected)
}
