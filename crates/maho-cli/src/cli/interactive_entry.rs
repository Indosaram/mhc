use std::{cell::{Cell, RefCell}, path::PathBuf, rc::Rc, sync::Arc};
use maho_tui::{components::editor::EditorTuiHost, terminal::{ProcessTerminal, Terminal}, tui::Component};

use super::shared_host::{is_truthy_env_flag, join_shared_host, should_join_shared_host, shared_host_setting_enabled, SharedHostMount, SHARED_HOST_ENABLE_ENV_SUFFIX};

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

/// senpi `main.ts`: join the shared session host before the interactive runtime is mounted, using
/// the policy in `core/shared-host-policy.ts`.
pub async fn mount_shared_host(session: &maho_core::agent_session::AgentSession, parsed: &super::args::Args) -> Option<SharedHostMount> {
    let enable_env = is_truthy_env_flag(
        maho_core::brand::env_value(SHARED_HOST_ENABLE_ENV_SUFFIX, &maho_core::config::current_env()).as_deref(),
    );
    let setting_enabled = session.with_settings_manager(shared_host_setting_enabled);
    if !should_join_shared_host(maho_core::project_trust::AppMode::Interactive, enable_env, setting_enabled) {
        return None;
    }
    let agent_dir = session.agent_dir();
    let model = session.model();
    let binding = maho_interactive::interactive_host_runtime::HostSessionBinding {
        session_path: session.session_file(),
        cwd: session.cwd(),
        agent_dir: PathBuf::from(&agent_dir),
        provider: Some(model.provider.clone()),
        model: Some(model.id.clone()),
        thinking_level: parsed.thinking.clone(),
    };
    Some(join_shared_host(binding, &agent_dir, None).await)
}

pub async fn run(session: Arc<maho_core::agent_session::AgentSession>, parsed: &super::args::Args, initial: super::initial_message::InitialMessageResult, extensions: Vec<maho_ext_api::LoadedExtension>, mut widget_requests: tokio::sync::mpsc::UnboundedReceiver<maho_interactive::interactive_extension_ui::UiRequest>) -> Result<(), String> {
    use maho_interactive::{interactive_mode::InteractiveMode, tui_renderer::{create_interactive_tui, InteractiveTuiOptions, TuiMode}};
    let shared_host = mount_shared_host(&session, parsed).await;
    if let Some(message) = shared_host.as_ref().and_then(SharedHostMount::warning_message) { eprintln!("{message}"); }
    let theme_setting = parsed.use_theme.clone().or_else(|| session.with_settings_manager(|settings| settings.get_string("theme")));
    let theme = super::startup_ui::resolve_startup_theme(theme_setting.as_deref(), std::env::var("COLORFGBG").ok().as_deref())?;
    let mut terminal = ProcessTerminal::default();
    let host = Rc::new(EditorHost { rows: Cell::new(usize::from(terminal.rows())), render: Cell::new(true) });
    let mut mode = InteractiveMode::new(session.clone(), theme.clone(), host.clone());
    if let Some(host) = shared_host.as_ref().and_then(|mount| mount.outcome.session_host()) {
        mode.set_session_host(host);
    }
    mode.rebuild_history();
    mode.bind_extensions().await;
    mode.use_registered_markdown_transformers(&extensions);
    mode.initialize_startup_header();
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
        let mut installed_native_renderers = Vec::new();
        while !mode.shutdown_requested {
            mode.drain_events();
            while let Ok(request) = widget_requests.try_recv() { mode.handle_ui_request(request); }
            let native = session.native_tool_renderers_snapshot::<(), serde_json::Value>().await;
            let patch = session.native_tool_renderers_snapshot::<maho_ext_gpt_apply_patch::preview_format::ApplyPatchRenderState, serde_json::Value>().await;
            let inventory: Vec<_> = native.iter().map(|(name, renderers)| (name.clone(), Arc::as_ptr(renderers) as usize))
                .chain(patch.iter().map(|(name, renderers)| (name.clone(), Arc::as_ptr(renderers) as usize))).collect();
            if inventory != installed_native_renderers {
                mode.install_native_tool_renderer_snapshot(native);
                for (name, renderers) in patch { mode.install_native_tool_renderers(name, renderers); }
                installed_native_renderers = inventory;
            }
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
    drop(shared_host);
    result.and(stopped)
}
