use std::{cell::{Cell, RefCell}, rc::Rc, sync::Arc};
use maho_tui::{components::editor::EditorTuiHost, terminal::{ProcessTerminal, Terminal}, tui::Component};

struct EditorHost { rows: Cell<usize>, render: Cell<bool> }
impl EditorTuiHost for EditorHost {
    fn request_render(&self) { self.render.set(true); }
    fn terminal_rows(&self) -> usize { self.rows.get() }
}

struct RenderedMode(Vec<String>);
impl Component for RenderedMode {
    fn render(&mut self, _width: usize) -> Vec<String> { self.0.clone() }
    fn invalidate(&mut self) {}
}

pub async fn run(session: Arc<maho_core::agent_session::AgentSession>, parsed: &super::args::Args, initial: super::initial_message::InitialMessageResult) -> Result<(), String> {
    use maho_interactive::{interactive_mode::InteractiveMode, tui_renderer::{create_interactive_tui, InteractiveTuiOptions, TuiMode}};
    let theme_setting = parsed.use_theme.clone().or_else(|| session.with_settings_manager(|settings| settings.get_string("theme")));
    let theme = super::startup_ui::resolve_startup_theme(theme_setting.as_deref(), std::env::var("COLORFGBG").ok().as_deref())?;
    let mut terminal = ProcessTerminal::default();
    let host = Rc::new(EditorHost { rows: Cell::new(usize::from(terminal.rows())), render: Cell::new(true) });
    let mut mode = InteractiveMode::new(session, theme.clone(), host.clone());
    mode.rebuild_history();
    mode.bind_extensions().await;
    let mut screen = create_interactive_tui(InteractiveTuiOptions {
        tui_mode: if parsed.tui_mode.as_deref() == Some("fullscreen") { TuiMode::Fullscreen } else { TuiMode::Regular },
        show_hardware_cursor: true, bottom_shortcut: String::new(),
    }, theme);
    let rendered = Rc::new(RefCell::new(RenderedMode(Vec::new())));
    let component: Rc<RefCell<dyn Component>> = rendered.clone();
    screen.base_mut().add_child(component.clone());
    screen.base_mut().set_focus(Some(component));
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = input.clone();
    screen.before_terminal_start(&mut terminal, false, false);
    terminal.start(Box::new(move |chunk| captured.borrow_mut().push(chunk.into())), Box::new(|| {}));
    let result = async {
        if let Some(text) = initial.initial_message { mode.submit(&text, maho_core::agent_session::PromptOptions { images: initial.initial_images, ..Default::default() }).await?; }
        for text in &parsed.messages { mode.submit(text, Default::default()).await?; }
        let clock = std::time::Instant::now();
        while !mode.shutdown_requested {
            host.rows.set(usize::from(terminal.rows()));
            let now = u64::try_from(clock.elapsed().as_millis()).map_err(|error| error.to_string())?;
            let chunks = std::mem::take(&mut *input.borrow_mut());
            for chunk in chunks { mode.handle_runtime_input(&chunk, now).await?; }
            rendered.borrow_mut().0 = mode.render(usize::from(terminal.columns()));
            screen.base_mut().request_render(false, now);
            screen.do_render(&mut terminal);
            terminal.pump(16).map_err(|error| error.to_string())?;
            tokio::task::yield_now().await;
        }
        Ok(())
    }.await;
    screen.before_terminal_stop(&mut terminal, false);
    let stopped = terminal.stop().map_err(|error| error.to_string());
    screen.after_terminal_stop(&mut terminal, false);
    result.and(stopped)
}
