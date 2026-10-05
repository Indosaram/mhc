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
    let theme = resolve_interactive_startup_theme(&session, parsed, theme_setting.as_deref()).await?;
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
    screen.set_render_root(component.clone());
    screen.base_mut().set_focus(Some(component));
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = input.clone();
    screen.before_terminal_start(&mut terminal, false, false);
    terminal.start(Box::new(move |chunk| captured.borrow_mut().push(chunk.into())), Box::new(|| {}));
    let result = async {
        if let Some(text) = initial.initial_message { mode.enqueue_submission(&text, initial.initial_images); }
        for text in &parsed.messages { mode.enqueue_submission(text, None); }
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
            mode.pump_turn().await;
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

/// senpi `createStartupTui`'s theme half, owned by this interactive entry: register the
/// package/CLI-resolved theme resources before the first frame, then resolve the configured name.
/// The package manager is async and its `ResolvedPaths::themes` had no production consumer, so the
/// resources are loaded here (via task-2's `startup_ui::load_theme_resources` /
/// `resolve_startup_theme_with_registered`) instead of being dropped. Diagnostics are reported, not
/// swallowed, and the custom themes directory keeps the native `<agent_dir>/themes` contract.
async fn resolve_interactive_startup_theme(
    session: &maho_core::agent_session::AgentSession,
    parsed: &super::args::Args,
    setting: Option<&str>,
) -> Result<maho_interactive::theme::Theme, String> {
    let cwd = session.cwd();
    let agent_dir = session.agent_dir();
    let mut diagnostics: Vec<String> = Vec::new();
    // senpi `loadStartupThemes(settingsManager)`: package-resolved theme resources first.
    let mut theme_paths: Vec<PathBuf> = Vec::new();
    {
        let mut settings = startup_theme_settings_manager(session);
        let manager = maho_core::package_manager::DefaultPackageManager::new(maho_core::package_manager::PackageManagerOptions {
            cwd: &cwd, agent_dir: &agent_dir, settings_manager: &mut settings,
        });
        match manager.resolve(None).await {
            Ok(paths) => theme_paths.extend(paths.themes.into_iter().filter(|resource| resource.enabled).map(|resource| PathBuf::from(resource.path))),
            Err(error) => diagnostics.push(format!("package themes: {error}")),
        }
    }
    // senpi `resolveCliPaths`: the `--theme` paths resolved against the launch cwd.
    let runtime_config = super::host_runtime::CliRuntimeConfiguration::from_parsed(parsed, &cwd, &agent_dir, maho_core::project_trust::AppMode::Interactive);
    theme_paths.extend(runtime_config.resolved_theme_paths().into_iter().map(PathBuf::from));
    let (registered, theme_diagnostics) = super::startup_ui::load_theme_resources(theme_paths, maho_interactive::theme::ColorMode::Truecolor);
    diagnostics.extend(theme_diagnostics);
    let custom_directory = PathBuf::from(crate::config::get_custom_themes_dir());
    let resolution = super::startup_ui::resolve_startup_theme_with_registered(
        setting, std::env::var("COLORFGBG").ok().as_deref(), &custom_directory, registered,
    )?;
    diagnostics.extend(resolution.diagnostics);
    for diagnostic in &diagnostics { eprintln!("theme: {diagnostic}"); }
    Ok(resolution.theme)
}

/// senpi `loadStartupThemes` (startup-ui.ts:65-75):
/// `SettingsManager.inMemory(settingsManager.getGlobalSettings(), { projectTrusted: false })`.
/// The package-resolved theme paths come from the session's **live** global settings (honouring
/// unsaved in-memory overrides), with project settings excluded.
fn startup_theme_settings_manager(session: &maho_core::agent_session::AgentSession) -> maho_core::settings_manager::SettingsManager {
    maho_core::settings_manager::SettingsManager::in_memory(session.with_settings_manager(|settings| settings.get_global().clone()), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `AgentSession` whose settings manager holds `global` **in memory**
    /// (`SettingsManager::in_memory`), with `agent_dir` on an isolated temp dir. Building the real
    /// session is what makes the regression exercise the production seam instead of a wrapper.
    fn theme_session(global: maho_core::settings_manager::Settings, agent_dir: &std::path::Path) -> maho_core::agent_session::AgentSession {
        use std::sync::Arc;
        use maho_core::agent_session::{AgentSession, AgentSessionConfig};
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions { providers: Some(Vec::new()), ..Default::default() });
        let stream_fn: maho_agent::types::StreamFn = Arc::new(|_, _, _| maho_ai::types::AssistantMessageEventStream::assistant());
        let cwd = agent_dir.to_string_lossy().into_owned();
        AgentSession::new(AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions { stream_fn: Some(stream_fn), ..Default::default() }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd, None, None),
            settings_manager: maho_core::settings_manager::SettingsManager::in_memory(global, false),
            cwd: cwd.clone(),
            agent_dir: Some(cwd),
            fallback_now: None, retry_random: None,
            scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(),
            model_runtime: Some(runtime), model_registry: None, uses_default_stream_function: Some(false),
            initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None,
            session_start_event: None, auto_title_sessions: Some(false),
        }).expect("session")
    }

    #[test]
    fn startup_theme_settings_seam_reads_live_global_and_excludes_project() {
        let root = tempfile::tempdir().expect("temp root");
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        // The disk carries a DIFFERENT global with no theme sources; the seam must not read it.
        std::fs::write(agent_dir.join("settings.json"), r#"{"theme":"disk-theme"}"#).expect("disk settings");

        let mut unsaved = maho_core::settings_manager::Settings::new();
        unsaved.insert("theme".to_owned(), serde_json::json!("unsaved-theme"));
        unsaved.insert("themes".to_owned(), serde_json::json!([root.path().join("global-themes").to_string_lossy()]));
        let session = theme_session(unsaved.clone(), &agent_dir);

        // The production seam behind `resolve_interactive_startup_theme`: the session's live global
        // settings, with project scope excluded.
        let manager = startup_theme_settings_manager(&session);
        assert_eq!(manager.get_global(), &unsaved, "the seam resolves from the session's live global settings");
        assert!(manager.get_global().get("themes").is_some(), "the unsaved global theme source is preserved");
        assert!(manager.get_project().is_empty(), "project theme sources are excluded");
        assert!(!manager.is_project_trusted(), "project scope is not trusted for startup themes");

        // A disk reload drops the unsaved global, so this fails if the seam reverts to
        // `SettingsManager::create` (it would carry the disk global, which has no theme sources).
        let disk = maho_core::settings_manager::SettingsManager::create(&session.cwd(), &agent_dir.to_string_lossy(), &root.path().to_string_lossy(), false);
        assert!(disk.get_global().get("themes").is_none(), "a disk-create manager cannot see the unsaved global theme source");
    }
}
