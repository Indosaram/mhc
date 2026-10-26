use maho_tui::{components::editor::EditorTuiHost, terminal::Terminal, tui::Component};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

use super::shared_host::{
    SHARED_HOST_ENABLE_ENV_SUFFIX, SharedHostMount, is_truthy_env_flag, join_shared_host,
    shared_host_setting_enabled, should_join_shared_host,
};

/// Longest an idle frame loop goes without rebuilding the mode's lines, for a change that reached
/// the mode without a repaint signal.
const IDLE_REBUILD_BACKSTOP_MS: u64 = 1_000;

struct EditorHost {
    rows: Cell<usize>,
    render: Cell<bool>,
    full_redraws: Cell<u64>,
}
impl EditorTuiHost for EditorHost {
    fn request_render(&self) {
        self.render.set(true);
    }
    fn terminal_rows(&self) -> usize {
        self.rows.get()
    }
    fn full_redraws(&self) -> u64 {
        self.full_redraws.get()
    }
}

struct RenderedMode(Vec<String>);
impl Component for RenderedMode {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
    fn invalidate(&mut self) {}
}

/// Waker for parked actions: records that a parked future made progress so the main loop keeps
/// polling it within the frame instead of waiting a full frame per await.
#[derive(Default)]
struct ParkedWake {
    woken: std::sync::atomic::AtomicBool,
}
impl std::task::Wake for ParkedWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Poll both native-renderer snapshot futures once. `None` when either is not ready: the runner
/// lock is not free, so the frame keeps the installed snapshot instead of awaiting it.
fn native_snapshots_if_lock_free<T, U>(
    native: impl std::future::Future<Output = T>,
    patch: impl std::future::Future<Output = U>,
) -> Option<(T, U)> {
    let native = poll_now(native)?;
    Some((native, poll_now(patch)?))
}

fn poll_now<T>(fut: impl std::future::Future<Output = T>) -> Option<T> {
    let mut fut = std::pin::pin!(fut);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match fut.as_mut().poll(&mut context) {
        std::task::Poll::Ready(value) => Some(value),
        std::task::Poll::Pending => None,
    }
}

/// A leaf component that renders a line buffer the render loop refreshes each frame. The fullscreen
/// chat viewport mounts two: the transcript document (inside its follow-end scroll view) and the
/// fixed input dock (pinned below it), so the dock stays visible once the document exceeds the
/// terminal height (senpi `chat-viewport.ts` `follow: "end"`).
struct LinesBuffer(Rc<RefCell<Vec<String>>>);
impl Component for LinesBuffer {
    fn render(&mut self, _width: usize) -> Vec<String> {
        self.0.borrow().clone()
    }
    fn invalidate(&mut self) {}
}

/// senpi `main.ts`: join the shared session host before the interactive runtime is mounted, using
/// the policy in `core/shared-host-policy.ts`.
pub async fn mount_shared_host(
    session: &maho_core::agent_session::AgentSession,
    parsed: &super::args::Args,
) -> Option<SharedHostMount> {
    let env = maho_core::config::current_env();
    let enable_env = is_truthy_env_flag(
        maho_core::brand::env_value(SHARED_HOST_ENABLE_ENV_SUFFIX, &env).as_deref(),
    );
    let setting_enabled = session.with_settings_manager(shared_host_setting_enabled);
    if !should_join_shared_host(
        maho_core::project_trust::AppMode::Interactive,
        enable_env,
        setting_enabled,
    ) {
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

#[allow(clippy::too_many_arguments)]
pub async fn run(
    session: Arc<maho_core::agent_session::AgentSession>,
    parsed: &super::args::Args,
    initial: super::initial_message::InitialMessageResult,
    extensions: Vec<maho_ext_api::LoadedExtension>,
    mut widget_requests: tokio::sync::mpsc::UnboundedReceiver<
        maho_interactive::interactive_extension_ui::UiRequest,
    >,
    migrated_providers: Vec<String>,
    model_fallback_message: Option<String>,
    startup_benchmark: bool,
) -> Result<(), String> {
    use maho_interactive::{
        interactive_mode::{InteractiveMode, InteractiveModeOptions},
        tui_renderer::TuiMode,
    };
    let (notice_sender, mut notices) = tokio::sync::mpsc::unbounded_channel::<StartupNotice>();
    let version_sender = notice_sender.clone();
    tokio::spawn(async move {
        if let Some(release) = crate::native_update::release::available_cached().await {
            let _ = version_sender.send(StartupNotice::NewVersion(release));
        }
    });
    let shared_host = mount_shared_host(&session, parsed).await;
    if let Some(message) = shared_host
        .as_ref()
        .and_then(SharedHostMount::warning_message)
    {
        maho_core::output_guard::maho_write_stderr(&format!("{message}\n"));
    }
    let theme_setting = parsed
        .use_theme
        .clone()
        .or_else(|| session.with_settings_manager(|settings| settings.get_string("theme")));
    // senpi `applyGrokNeoThemeFallback`: grok-night until the user chooses a theme (never persisted).
    let theme_setting = theme_setting.or_else(|| parsed.grok_neo.then(|| "grok-night".to_owned()));
    let (theme, registered_themes) =
        resolve_interactive_startup_theme(&session, parsed, theme_setting.as_deref()).await?;
    let mut terminal = maho_interactive::tui_renderer::create_process_terminal();
    let host = Rc::new(EditorHost {
        rows: Cell::new(usize::from(terminal.rows())),
        render: Cell::new(true),
        full_redraws: Cell::new(0),
    });
    // senpi main.ts:1192-1195 `autoTrustOnReloadCwd`.
    let session_cwd = session.with_session_manager(|manager| manager.cwd().to_owned());
    let auto_trust_on_reload_cwd = (parsed.project_trust_override.is_none()
        && !maho_core::trust_manager::has_trust_requiring_project_resources(&session_cwd, None))
    .then_some(session_cwd);
    // senpi `interactiveAuthNotices`: shown once at startup, then recorded as seen.
    let auth_storage = session.model_registry().auth_storage.clone();
    let (auth_messages, auth_notice_ids) = super::runtime::interactive_auth_notices(&auth_storage);
    let auth_notices = (!auth_messages.is_empty() || !auth_notice_ids.is_empty()).then(|| {
        maho_interactive::interactive_mode::AuthNotices {
            messages: auth_messages,
            mark_seen: Some(Box::new(move || {
                auth_storage.mark_notices_seen(&auth_notice_ids)
            })),
        }
    });
    let mut mode = InteractiveMode::new_with_options(
        session.clone(),
        theme.clone(),
        host.clone(),
        InteractiveModeOptions {
            verbose: parsed.verbose,
            chrome: parsed
                .grok_neo
                .then(|| maho_interactive::grok::chrome::GrokChrome::new(theme.clone())),
            migrated_providers,
            model_fallback_message,
            auth_notices,
            auto_trust_on_reload_cwd,
            initial_title_prompt: initial.initial_title_prompt.clone(),
        },
    );
    *mode
        .extension_ui
        .registered_themes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = registered_themes;
    *mode
        .extension_ui
        .builtin_theme_directory
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(PathBuf::from(crate::config::get_themes_dir()));
    if let Some(host) = shared_host
        .as_ref()
        .and_then(|mount| mount.outcome.session_host())
    {
        mode.set_session_host(host);
    }
    // senpi `options.tuiMode ?? settingsManager.getTuiMode()`: the flag, then the persisted setting.
    let fullscreen = parsed
        .tui_mode
        .clone()
        .or_else(|| session.with_settings_manager(|settings| settings.get_string("tuiMode")))
        .as_deref()
        == Some("fullscreen");
    let document_lines = Rc::new(RefCell::new(Vec::<String>::new()));
    let dock_lines = Rc::new(RefCell::new(Vec::<String>::new()));
    let rendered = Rc::new(RefCell::new(RenderedMode(Vec::new())));
    // senpi `onRightClickPaste`: the alternate screen flags the press; the loop reads the clipboard.
    let right_click_paste = Rc::new(Cell::new(false));
    let MountedScreen {
        mut screen,
        mut component,
        mut transcript,
    } = mount_screen(
        &session,
        if fullscreen {
            TuiMode::Fullscreen
        } else {
            TuiMode::Regular
        },
        theme,
        mode.terminal_settings().0,
        &right_click_paste,
        &document_lines,
        &dock_lines,
        &rendered,
    );
    let mut fullscreen = fullscreen;
    // senpi focuses the editor (`ui.setFocus(this.editor)`); a focused editor emits the cursor
    // marker the renderer turns into the per-frame hardware cursor restore (row move + absolute
    // column). The mounted root here is a line buffer, so focus is handed to the mode directly.
    maho_tui::tui::Component::focusable_set(&mut mode, true);
    let input = Rc::new(RefCell::new(Vec::<String>::new()));
    let captured = input.clone();
    // senpi `registerSignalHandlers`: SIGTERM/SIGHUP shut down gracefully (terminal restore, then
    // the caller's `session_shutdown`); SIGWINCH re-reports the width to a mounted shared host.
    #[cfg(unix)]
    let mut signals = {
        use tokio::signal::unix::{SignalKind, signal};
        let install = |kind: SignalKind| signal(kind).map_err(|error| error.to_string());
        (
            install(SignalKind::terminate())?,
            install(SignalKind::hangup())?,
            install(SignalKind::window_change())?,
        )
    };
    // senpi `run()`: the version check and tmux probe start asynchronously; their notices land in
    // the transcript when they resolve.
    tokio::spawn(async move {
        if let Some(warning) = maho_interactive::interactive_mode::check_tmux_keyboard_setup().await
        {
            let _ = notice_sender.send(StartupNotice::Warning(warning));
        }
    });
    // senpi `TuiAltScreen` `mouse: terminalMouseMode !== "off"`; multiplexers get button-motion
    // tracking. The `TuiBase.start()` sequence brackets `terminal.start`.
    let mut mouse_enabled =
        session.with_settings_manager(|settings| settings.get_terminal_mouse()) != "off";
    screen.base_mut().prepare_start();
    screen.before_terminal_start(
        &mut terminal,
        mouse_enabled,
        maho_tui::tui_alt_screen::prefers_button_motion_mouse(),
    );
    terminal.start(
        Box::new(move |chunk| captured.borrow_mut().push(chunk.into())),
        Box::new(|| {}),
    );
    screen.base_mut().after_terminal_start(&mut terminal);
    mode.resume_question_mouse_capture(&mut screen, &mut terminal);
    mode.bind_terminal_theme(&mut screen, theme_setting);
    // senpi `init()`: paint the header, editor and footer before the expensive history rebuild and
    // extension binding, so a large session shows the prompt immediately.
    mode.mount_startup_chrome();
    if fullscreen {
        let (document, dock) = mode.render_split(usize::from(terminal.columns()));
        *document_lines.borrow_mut() = document;
        *dock_lines.borrow_mut() = dock;
    } else {
        rendered.borrow_mut().0 = mode.render(usize::from(terminal.columns()));
    }
    screen.do_render(&mut terminal);
    // An installed fd is found with a local PATH/bin stat, so `@` mentions work from the first
    // keystroke; the background check below only re-installs the provider when it changes the path.
    let local_fd_path = crate::utils::tools_manager::get_tool_path(crate::utils::tools_manager::Tool::Fd);
    if local_fd_path.is_some() {
        mode.set_fd_path(local_fd_path.clone());
    }
    mode.rebuild_history();
    // senpi `init()` awaits `rebindCurrentSession()` with the editor already focused and its
    // event loop live, so keystrokes typed while extensions bind (the MCP startup race alone
    // can take 250ms) echo at once. Serve input and paint here until the binding settles.
    // An input or terminal error here is deferred to the main loop's result, so the terminal is
    // still restored on the way out (a hangup shuts down like the main loop does).
    let mut binding_error: Option<String> = None;
    {
        let binding = mode.start_extension_binding();
        tokio::pin!(binding);
        let clock = std::time::Instant::now();
        loop {
            tokio::select! { biased; () = &mut binding => break, () = tokio::task::yield_now() => {} }
            if binding_error.is_some() || mode.shutdown_requested {
                binding.as_mut().await;
                break;
            }
            mode.drain_events();
            let now = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
            let chunks = std::mem::take(&mut *input.borrow_mut());
            let had_input = !chunks.is_empty();
            for chunk in chunks {
                if let Some(data) = route_terminal_input(&mut screen, &mut terminal, &mut mode, &component, &chunk, now)
                    && let Err(error) = mode.handle_filtered_runtime_input(&data, now).await
                {
                    binding_error = Some(error);
                }
            }
            if fullscreen {
                let (document, dock) = mode.render_split(usize::from(terminal.columns()));
                *document_lines.borrow_mut() = document;
                *dock_lines.borrow_mut() = dock;
            } else {
                rendered.borrow_mut().0 = mode.render(usize::from(terminal.columns()));
            }
            if had_input { screen.base_mut().request_render_from(false, "input", now); }
            if screen.base_mut().take_due_render(now) { screen.do_render(&mut terminal); }
            match terminal.pump(screen.base().min_render_interval_ms()) {
                Ok(()) => {}
                Err(error) if error.is_hangup() => {
                    #[cfg(unix)]
                    crate::utils::shell::kill_tracked_detached_children();
                    mode.shutdown_requested = true;
                }
                Err(error) => binding_error = Some(error.to_string()),
            }
        }
    }
    mode.finish_extension_binding().await;
    mode.use_registered_markdown_transformers(&extensions);
    mode.initialize_startup_resources();
    mode.show_startup_model_warnings();
    // senpi: ensure fd and rg (download when missing, honouring offline mode) after the header is up;
    // fd feeds the `@` file-mention search. A missing tool is downloaded over the network, so the
    // check runs as a task and the input loop applies its result: keystrokes keep echoing while it
    // runs (senpi's event loop serves stdin during the same await).
    let mut managed_tools = Some(tokio::spawn(async {
        use crate::utils::tools_manager::{Tool, ensure_tool};
        let (mut fd_statuses, mut rg_statuses) = (Vec::new(), Vec::new());
        let (fd_path, _rg_path) = tokio::join!(
            ensure_tool(Tool::Fd, |status| fd_statuses.push(status)),
            ensure_tool(Tool::Rg, |status| rg_statuses.push(status)),
        );
        fd_statuses.extend(rg_statuses);
        (fd_path, fd_statuses)
    }));
    mode.show_startup_warnings();
    if let Some(notice) = crate::legacy_pi_edits::legacy_pi_edit_startup_notice(
        std::path::Path::new(&session.agent_dir()),
        std::path::Path::new(&maho_core::config::home_dir()),
    ) {
        mode.show_warning(&notice);
    }
    let mut progress_active = false;
    let result = async {
        if let Some(error) = binding_error { return Err(error); }
        // senpi main.ts:1355-1370 `PI_STARTUP_BENCHMARK`: once the TUI is initialized, give stdin a
        // brief chance to consume terminal query replies, then stop instead of running the loop.
        if startup_benchmark {
            maho_core::timings::time("interactiveMode.init", maho_core::timings::TimingLabel::Main);
            rendered.borrow_mut().0 = mode.render(usize::from(terminal.columns()));
            // senpi's `start()` requests an ordinary (unforced) first frame; paint it now.
            screen.do_render(&mut terminal);
            terminal.pump(150).map_err(|error| error.to_string())?;
            return Ok(());
        }
        let progress_active = &mut progress_active;
        if let Some(text) = initial.initial_message { mode.enqueue_initial_message(&text, initial.initial_images); }
        for text in &parsed.messages { mode.enqueue_submission(text, None); }
        let clock = std::time::Instant::now();
        let parked_wake = std::sync::Arc::new(ParkedWake::default());
        let parked_waker = std::task::Waker::from(parked_wake.clone());
        let mut installed_native_renderers = Vec::new();
        let mut installed_renderer_revision: Option<(u64, u64)> = None;
        // senpi `applyTerminalTitle` / `setProgress`: written only when the value changes.
        let mut applied_title: Option<String> = None;
        let mut last_size = (terminal.columns(), terminal.rows());
        let mut last_rebuild_ms = 0_u64;
        let mut right_click_target: Option<Rc<RefCell<dyn Component>>> = None;
        while !mode.shutdown_requested {
            #[cfg(unix)]
            {
                let mut context = std::task::Context::from_waker(std::task::Waker::noop());
                let (terminate, hangup, resize) = &mut signals;
                if terminate.poll_recv(&mut context).is_ready() || hangup.poll_recv(&mut context).is_ready() {
                    crate::utils::shell::kill_tracked_detached_children();
                    mode.shutdown_requested = true;
                    break;
                }
                if resize.poll_recv(&mut context).is_ready()
                    && let Some(maho_interactive::interactive_host_runtime::InteractiveHostRuntimeOutcome::Remote(runtime)) = shared_host.as_ref().map(|mount| &mount.outcome) {
                    runtime.set_client_info(f64::from(terminal.columns())).await;
                }
            }
            while let Ok(notice) = notices.try_recv() {
                match notice {
                    StartupNotice::NewVersion(release) => {
                        // senpi `BRAND?.update?.command ?? `${APP_NAME} update``. The shell
                        // command is the executable (`app_command`, `mhc`), not the display name.
                        let action = maho_core::brand::brand_profile()
                            .update
                            .map(|channel| channel.command)
                            .unwrap_or_else(|| format!("{} update", maho_core::config::app_command()));
                        let url = format!("https://github.com/Indosaram/mhc/releases/tag/{}", release.tag);
                        mode.show_new_version_notification(&release.version, &action, &url);
                    }
                    StartupNotice::Warning(warning) => mode.show_warning(&warning),
                }
            }
            if managed_tools.as_ref().is_some_and(tokio::task::JoinHandle::is_finished)
                && let Some(task) = managed_tools.take()
            {
                match task.await {
                    Ok((fd_path, statuses)) => {
                        for status in statuses { mode.show_managed_tool_status(status.kind, &status.message); }
                        if fd_path != local_fd_path { mode.set_fd_path(fd_path); }
                    }
                    Err(error) => mode.show_managed_tool_status("warning", &format!("Failed to check fd/rg: {error}")),
                }
            }
            mode.drain_events();
            while let Ok(request) = widget_requests.try_recv() { mode.handle_ui_request(request); }
            // Rebuild the renderer snapshots only when the bound runner or its live tools changed:
            // walking every extension's tools and renderers each frame kept an idle session busy.
            // `None` (no runner yet, or its lock is held this tick) keeps the installed set.
            if let Some(revision) = session.tool_renderers_revision() && installed_renderer_revision != Some(revision) {
                // Do not await the snapshot when the runner lock is not free. A pending poll means
                // the lock was taken after the revision try_lock; keep the installed set and retry.
                if let Some((native, patch)) = native_snapshots_if_lock_free(
                    session.native_tool_renderers_snapshot::<(), serde_json::Value>(),
                    session.native_tool_renderers_snapshot::<maho_ext_gpt_apply_patch::preview_format::ApplyPatchRenderState, serde_json::Value>(),
                ) {
                    installed_renderer_revision = Some(revision);
                    let inventory: Vec<_> = native.iter().map(|(name, renderers)| (name.clone(), Arc::as_ptr(renderers) as usize))
                        .chain(patch.iter().map(|(name, renderers)| (name.clone(), Arc::as_ptr(renderers) as usize))).collect();
                    if inventory != installed_native_renderers {
                        mode.install_native_tool_renderer_snapshot(native);
                        for (name, renderers) in patch { mode.install_native_tool_renderers(name, renderers); }
                        installed_native_renderers = inventory;
                    }
                }
            }
            host.rows.set(usize::from(terminal.rows()));
            let now = u64::try_from(clock.elapsed().as_millis()).map_err(|error| error.to_string())?;
            let chunks = std::mem::take(&mut *input.borrow_mut());
            let had_input = !chunks.is_empty();
            for chunk in chunks {
                if let Some(data) = route_terminal_input(&mut screen, &mut terminal, &mut mode, &component, &chunk, now) {
                    mode.set_selection_copy_available(!screen.get_copy_on_select() && screen.has_active_selection());
                    mode.handle_filtered_runtime_input(&data, now).await?;
                }
            }
            // senpi `handleRightClickPaste`: paste into the component that was focused at the
            // press, unless focus moved while the clipboard was read.
            if let Some(text) = mode.take_parked_clipboard_text() {
                let target = right_click_target.take();
                if let (Some(text), Some(target)) = (text, target)
                    && screen.base().get_focused_component().is_some_and(|focused| Rc::ptr_eq(&focused, &target))
                {
                    let paste = format!("\x1b[200~{text}\x1b[201~");
                    if Rc::ptr_eq(&target, &component) { mode.handle_filtered_runtime_input(&paste, now).await?; } else { target.borrow_mut().handle_input(&paste); }
                    screen.base_mut().request_render(false, now);
                }
            }
            if right_click_paste.replace(false)
                && let Some(target) = screen.base().get_focused_component()
            {
                right_click_target = Some(target);
                mode.park_external_clipboard_text(crate::utils::clipboard::read_clipboard_text());
            }
            // senpi `handleCopyCommand({ preferSelection: true })`: the selection copy needs the terminal.
            if mode.take_pending_selection_copy() { screen.copy_active_selection_to_clipboard(&mut terminal); }
            if let Some(request) = mode.take_terminal_request() {
                // senpi `handleCtrlZ` / `handleOpenExternalEditor`: release the terminal (and the
                // stderr takeover), run the step, then restart and force a full redraw.
                mode.pause_question_mouse_capture(&mut screen, &mut terminal);
                screen.base_mut().begin_stop(&mut terminal);
                screen.before_terminal_stop(&mut terminal, mouse_enabled);
                screen.base_mut().finish_stop_output(&mut terminal, fullscreen);
                terminal.stop().map_err(|error| error.to_string())?;
                screen.after_terminal_stop(&mut terminal, false);
                maho_interactive::interactive_stderr_guard::restore_interactive_stderr();
                match request {
                    maho_interactive::interactive_mode::TerminalRequest::ExternalEditor => mode.open_external_editor().await,
                    maho_interactive::interactive_mode::TerminalRequest::Suspend => suspend_process()?,
                }
                maho_interactive::interactive_stderr_guard::take_over_interactive_stderr();
                screen.base_mut().reset_for_stop();
                screen.base_mut().prepare_start();
                screen.before_terminal_start(&mut terminal, mouse_enabled, maho_tui::tui_alt_screen::prefers_button_motion_mouse());
                let captured = input.clone();
                terminal.start(Box::new(move |chunk| captured.borrow_mut().push(chunk.into())), Box::new(|| {}));
                screen.base_mut().after_terminal_start(&mut terminal);
                mode.resume_question_mouse_capture(&mut screen, &mut terminal);
                screen.base_mut().invalidate();
                mode.mark_frame_dirty();
                screen.base_mut().request_render(true, now);
            }
            mode.sync_question_mouse_capture(&mut screen, &mut terminal);
            // senpi `switchTuiMode` for `/settings` `tuiMode` / `terminalMouse`: refused while an
            // overlay is open; otherwise the setting is saved and the renderer is recreated in
            // place (the old one keeps the screen), keeping the mode mounted and focused.
            let current_mode = if screen.is_regular() { TuiMode::Regular } else { TuiMode::Fullscreen };
            if let Some((next_mode, next_mouse)) = mode.pending_terminal_setting(current_mode) {
                let next_mouse_enabled = next_mouse != "off";
                if next_mode == current_mode && next_mouse_enabled == mouse_enabled {
                    // senpi `switchTuiMode` returns early for the mounted mode; lease changes
                    // (`whilePending` <-> `always`) follow in `sync_question_mouse_capture`.
                    mode.commit_terminal_setting();
                } else if screen.base().has_overlay() {
                    mode.reject_terminal_setting(current_mode);
                } else if mode.commit_terminal_setting() {
                    mode.pause_question_mouse_capture(&mut screen, &mut terminal);
                    screen.base_mut().begin_stop(&mut terminal);
                    screen.before_terminal_stop(&mut terminal, mouse_enabled);
                    screen.base_mut().finish_stop_output(&mut terminal, true);
                    screen.after_terminal_stop(&mut terminal, true);
                    screen.base_mut().reset_for_stop();
                    mouse_enabled = next_mouse_enabled;
                    let mounted = mount_screen(&session, next_mode, mode.active_theme().clone(), screen.base().get_show_hardware_cursor(), &right_click_paste, &document_lines, &dock_lines, &rendered);
                    (screen, component, transcript) = (mounted.screen, mounted.component, mounted.transcript);
                    fullscreen = next_mode == TuiMode::Fullscreen;
                    screen.base_mut().prepare_start();
                    screen.before_terminal_start(&mut terminal, mouse_enabled, maho_tui::tui_alt_screen::prefers_button_motion_mouse());
                    screen.base_mut().after_terminal_start(&mut terminal);
                    mode.resume_question_mouse_capture(&mut screen, &mut terminal);
                    mode.rebind_terminal_theme(&mut screen, &mut terminal);
                    screen.base_mut().invalidate();
                    mode.mark_frame_dirty();
                    // senpi `switchTuiMode` only calls `nextUi.start()`, whose render is unforced.
                    screen.base_mut().request_render(false, now);
                }
            }
            // senpi `applyFullscreenScrollbarSetting`.
            if let Some(transcript) = &transcript { transcript.borrow_mut().set_scrollbar(fullscreen_scrollbar(&session)); }
            mode.poll_terminal_theme(&mut screen, &mut terminal);
            mode.pump_turn().await;
            // senpi `showHardwareCursor` / `clearOnShrink` (re-applied on settings changes), the
            // OSC 9;4 progress indicator and the window title.
            let (show_hardware_cursor, clear_on_shrink, _) = mode.terminal_settings();
            screen.base_mut().set_show_hardware_cursor(show_hardware_cursor);
            screen.base_mut().set_clear_on_shrink(clear_on_shrink);
            let progress = mode.terminal_progress_active();
            if progress != *progress_active { terminal.set_progress(progress); *progress_active = progress; }
            let title = mode.terminal_window_title();
            if applied_title.as_deref() != Some(title.as_str()) { terminal.set_title(&title); applied_title = Some(title); }
            // senpi repaints only on `requestRender`: the mode's lines are rebuilt when input, a
            // resize, a host repaint request or a mode state change asks for it, or while a
            // time-driven element runs. A long idle transcript is not re-laid-out every frame; a
            // once-per-second rebuild backs up any change that reached the mode without a signal.
            let size = (terminal.columns(), terminal.rows());
            let resized = size != last_size;
            last_size = size;
            let host_requested = host.render.replace(false);
            let backstop = now.saturating_sub(last_rebuild_ms) >= IDLE_REBUILD_BACKSTOP_MS;
            let mode_requested = mode.take_frame_rebuild(clock.elapsed().as_secs_f64() * 1000.0);
            let mut content_changed = false;
            if had_input || resized || host_requested || mode_requested || backstop {
                last_rebuild_ms = now;
                if fullscreen {
                    let (document, dock) = mode.render_split(usize::from(terminal.columns()));
                    if *document_lines.borrow() != document { *document_lines.borrow_mut() = document; content_changed = true; }
                    if *dock_lines.borrow() != dock { *dock_lines.borrow_mut() = dock; content_changed = true; }
                } else {
                    let lines = mode.render(usize::from(terminal.columns()));
                    let mut rendered = rendered.borrow_mut();
                    if rendered.0 != lines { rendered.0 = lines; content_changed = true; }
                }
            }
            // senpi `applySmoothStreamingRenderFps` + `requestRender(false, "input")`: the frame
            // cap follows the smooth-streaming FPS, and keystroke frames skip the throttle.
            screen.poll_timers(now);
            screen.tick(now);
            // `/settings` persists fullscreenCopyOnSelect; senpi pushes it with setCopyOnSelect.
            screen.set_copy_on_select(session.with_settings_manager(|settings| settings.get_bool("fullscreenCopyOnSelect")).unwrap_or(true));
            screen.base_mut().set_max_render_fps(mode.smooth_streaming_render_fps());
            if had_input { screen.base_mut().request_render_from(false, "input", now); } else if content_changed || resized || host_requested { screen.base_mut().request_render(false, now); }
            if screen.base_mut().take_due_render(now) {
                screen.do_render(&mut terminal);
                host.full_redraws.set(screen.base_mut().full_redraws());
            }
            // First poll of a turn started this iteration, and any action parked so this frame
            // could paint, run only after paint. Input is read next (`terminal.pump`).
            parked_wake.woken.store(false, std::sync::atomic::Ordering::SeqCst);
            mode.poll_after_paint(&parked_waker);
            // Parked work (`/new`, `/resume`, `/fork`, reload) is a chain of awaits; re-polling it
            // once per frame cost one frame of stdin wait per await. Keep advancing it while its
            // waker fires (the runtime's timers and IO run at each yield), up to one frame of
            // budget, then paint and read input.
            let frame_ms = screen.base().min_render_interval_ms();
            let budget = std::time::Instant::now() + std::time::Duration::from_millis(frame_ms);
            while mode.has_parked_actions() && std::time::Instant::now() < budget {
                tokio::task::yield_now().await;
                if !parked_wake.woken.swap(false, std::sync::atomic::Ordering::SeqCst) { break; }
                mode.poll_turn_with(&parked_waker);
            }
            match terminal.pump(frame_ms) {
                Ok(()) => {}
                // The terminal closed without a SIGHUP reaching this process (the PTY master went
                // away): shut down like the hangup signal instead of polling a dead stdin forever.
                Err(error) if error.is_hangup() => {
                    #[cfg(unix)]
                    crate::utils::shell::kill_tracked_detached_children();
                    mode.shutdown_requested = true;
                    break;
                }
                Err(error) => return Err(error.to_string()),
            }
            tokio::task::yield_now().await;
        }
        Ok(())
    }.await;
    // senpi `shutdown`: drain in-flight Kitty key release events before stopping, so escape
    // sequences cannot leak to the parent shell over slow SSH (signal and interactive quit alike).
    terminal.drain_input(1000, 50);
    // senpi `stop()`: clear the progress indicator.
    if progress_active {
        terminal.set_progress(false);
    }
    // senpi `stopInteractiveTui`: the "transcript" exit output switches fullscreen to the regular
    // renderer (without starting it) and renders once, so the transcript lands in scrollback.
    if fullscreen && (mode.fatal_runtime_error || mode.fullscreen_exit_output() == maho_interactive::components::settings_selector::FullscreenExitOutput::Transcript) {
        while screen.base().has_overlay() { screen.base_mut().hide_overlay(); }
        let show_hardware_cursor = screen.base().get_show_hardware_cursor();
        let clear_on_shrink = screen.base().get_clear_on_shrink();
        mode.pause_question_mouse_capture(&mut screen, &mut terminal);
        screen.base_mut().begin_stop(&mut terminal);
        screen.before_terminal_stop(&mut terminal, mouse_enabled);
        screen.base_mut().finish_stop_output(&mut terminal, true);
        screen.after_terminal_stop(&mut terminal, true);
        screen.base_mut().reset_for_stop();
        screen = mount_screen(&session, TuiMode::Regular, mode.active_theme().clone(), show_hardware_cursor, &right_click_paste, &document_lines, &dock_lines, &rendered).screen;
        screen.base_mut().set_clear_on_shrink(clear_on_shrink);
        fullscreen = false;
        rendered.borrow_mut().0 = mode.render(usize::from(terminal.columns()));
        screen.do_render(&mut terminal);
    }
    // senpi `stopInteractiveTui` -> `TuiBase.stop({ preserveScreen: fullscreen })`: release mouse
    // leases, disable color-scheme notifications, hand terminal modes back, park the cursor below
    // the last frame, show it.
    mode.pause_question_mouse_capture(&mut screen, &mut terminal);
    screen.base_mut().begin_stop(&mut terminal);
    screen.before_terminal_stop(&mut terminal, mouse_enabled);
    screen
        .base_mut()
        .finish_stop_output(&mut terminal, fullscreen);
    let stopped = terminal.stop().map_err(|error| error.to_string());
    screen.after_terminal_stop(&mut terminal, fullscreen);
    screen.base_mut().reset_for_stop();
    drop(shared_host);
    if startup_benchmark {
        // senpi: `interactiveMode.stop(); stopThemeWatcher(); printTimings(); process.exit(0)`.
        maho_interactive::interactive_stderr_guard::restore_interactive_stderr();
        maho_core::timings::print_timings();
        result.and(stopped)?;
        std::process::exit(0);
    }
    // senpi `handleFatalRuntimeError`: `stop("transcript")` then `process.exit(1)`.
    if mode.fatal_runtime_error {
        maho_interactive::interactive_stderr_guard::restore_interactive_stderr();
        result.and(stopped)?;
        std::process::exit(1);
    }
    // senpi `shutdown`: after the terminal stops, tell the user how to reopen this session.
    let resume_command = session.with_session_manager(|manager| {
        maho_interactive::interactive_mode::format_resume_command(
            manager,
            std::io::IsTerminal::is_terminal(&std::io::stdout()),
        )
    });
    if let Some(command) = resume_command {
        maho_core::output_guard::maho_write_stdout(&format!(
            "\x1b[2mTo resume this session:\x1b[22m {command}\n"
        ));
    }
    result.and(stopped)
}

/// A startup check result delivered to the render loop (senpi's `.then` callbacks in `run()`).
enum StartupNotice {
    NewVersion(crate::native_update::release::Release),
    Warning(String),
}

/// The renderer the interactive loop draws into, with its mounted root and, in fullscreen, the
/// transcript scroll view.
struct MountedScreen {
    screen: maho_interactive::tui_renderer::InteractiveTui,
    component: Rc<RefCell<dyn Component>>,
    transcript: Option<Rc<RefCell<maho_tui::components::scroll_view::ScrollView>>>,
}

/// senpi `settingsManager.getFullscreenScrollbar()`.
fn fullscreen_scrollbar(
    session: &maho_core::agent_session::AgentSession,
) -> maho_tui::components::scroll_view::ScrollViewScrollbar {
    use maho_tui::components::scroll_view::ScrollViewScrollbar;
    match session
        .with_settings_manager(|settings| settings.get_string("fullscreenScrollbar"))
        .as_deref()
    {
        Some("always") => ScrollViewScrollbar::Always,
        Some("hidden") => ScrollViewScrollbar::Hidden,
        _ => ScrollViewScrollbar::Auto,
    }
}

/// senpi `createInteractiveTui` + `mountInteractiveTui` for this entry: build the renderer for
/// `tui_mode` and mount the line buffers the loop fills each frame.
#[allow(clippy::too_many_arguments)]
fn mount_screen(
    session: &maho_core::agent_session::AgentSession,
    tui_mode: maho_interactive::tui_renderer::TuiMode,
    theme: maho_interactive::theme::Theme,
    show_hardware_cursor: bool,
    right_click_paste: &Rc<Cell<bool>>,
    document_lines: &Rc<RefCell<Vec<String>>>,
    dock_lines: &Rc<RefCell<Vec<String>>>,
    rendered: &Rc<RefCell<RenderedMode>>,
) -> MountedScreen {
    use maho_interactive::tui_renderer::{InteractiveTuiOptions, TuiMode, create_interactive_tui};
    let mut screen = create_interactive_tui(
        InteractiveTuiOptions {
            tui_mode,
            show_hardware_cursor,
            bottom_shortcut: String::new(),
        },
        theme,
    );
    // senpi interactive-mode.ts `logDirectory: getAgentDir()`.
    screen
        .base_mut()
        .set_log_directory(PathBuf::from(maho_core::config::get_agent_dir()));
    // senpi tui-renderer.ts: fullscreen opens OSC 8 links in the browser, copies selections
    // through the platform clipboard (flashing the real outcome) and honors fullscreenCopyOnSelect.
    if let maho_interactive::tui_renderer::InteractiveTui::Fullscreen(alt) = &mut screen {
        alt.set_open_url(Some(Rc::new(|url: &str| {
            crate::utils::open_browser::open_browser(url)
        })));
        let paste = right_click_paste.clone();
        alt.set_on_right_click_paste(Some(Rc::new(move || paste.set(true))));
        alt.set_copy_selection(Some(Rc::new(|text: String| {
            let (sender, receiver) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let copied = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .is_ok_and(|runtime| {
                        runtime
                            .block_on(crate::utils::clipboard::copy_to_clipboard(&text))
                            .is_ok()
                    });
                let _ = sender.send(copied);
            });
            receiver
        })));
        alt.set_copy_on_select(
            session
                .with_settings_manager(|settings| settings.get_bool("fullscreenCopyOnSelect"))
                .unwrap_or(true),
        );
    }
    // senpi mounts the fullscreen render root as the chat viewport (interactive-mode.ts
    // `mountInteractiveTui` -> `setLayoutRoot(this.fullscreenLayoutRoot)`), whose transcript scroll
    // view follows the end (chat-viewport.ts `follow: "end"`). Mounting the flat `mode.render()`
    // line list instead discards that scroll view, so the alternate screen lays the document out
    // top-anchored and the input dock falls below the fold once the document exceeds the terminal
    // height. The two `LinesBuffer`s reproduce the split: the loop fills the document and dock
    // buffers separately, the transcript follows the end, and the dock stays pinned.
    let (component, transcript): (Rc<RefCell<dyn Component>>, _) =
        if tui_mode == TuiMode::Fullscreen {
            let empty = || {
                Rc::new(RefCell::new(
                    maho_tui::components::text::Text::with_padding("", 0, 0),
                )) as Rc<RefCell<dyn Component>>
            };
            let viewport = maho_interactive::chat_viewport::create_chat_viewport(
                maho_interactive::chat_viewport::ChatViewportOptions {
                    document: Rc::new(RefCell::new(LinesBuffer(document_lines.clone()))),
                    editor: Rc::new(RefCell::new(LinesBuffer(dock_lines.clone()))),
                    pending_messages: empty(),
                    status: empty(),
                    footer: empty(),
                    hook_status: None,
                    widgets_above: None,
                    widgets_below: None,
                    scrollbar: Some(fullscreen_scrollbar(session)),
                    scrollbar_track_style: None,
                    scrollbar_thumb_style: None,
                },
            );
            (
                viewport.root as Rc<RefCell<dyn Component>>,
                Some(viewport.transcript),
            )
        } else {
            (rendered.clone() as Rc<RefCell<dyn Component>>, None)
        };
    screen.set_render_root(component.clone());
    screen.base_mut().set_focus(Some(component.clone()));
    MountedScreen {
        screen,
        component,
        transcript,
    }
}

/// senpi `TuiBase.handleTerminalInput` for the mounted interactive renderer: terminal protocol
/// replies are consumed, the renderer's input listeners (mouse, focus reports and, in fullscreen,
/// the `tui.altScreen.*` viewport keys) and then the extension terminal-input listeners run before
/// focus delivery whichever component is focused, Shift+Ctrl+D writes the debug log
/// (`ui.onDebug = handleDebugCommand`), overlay focus is restored, and input for a focused overlay
/// is delivered through the renderer. Returns the (listener-filtered) input for the interactive
/// mode when the mounted root holds focus.
fn route_terminal_input(
    screen: &mut maho_interactive::tui_renderer::InteractiveTui,
    terminal: &mut dyn Terminal,
    mode: &mut maho_interactive::interactive_mode::InteractiveMode,
    root: &Rc<RefCell<dyn Component>>,
    chunk: &str,
    now_ms: u64,
) -> Option<String> {
    let data = screen.base_mut().consume_terminal_protocol_input(chunk)?;
    if screen.handle_mouse_input(&data, i64::try_from(now_ms).unwrap_or(i64::MAX), terminal) {
        return None;
    }
    let root_focused = |screen: &maho_interactive::tui_renderer::InteractiveTui| {
        screen
            .base()
            .get_focused_component()
            .is_some_and(|focused| Rc::ptr_eq(&focused, root))
    };
    if root_focused(&*screen)
        && let maho_interactive::tui_renderer::InteractiveTui::Fullscreen(alt) = &mut *screen
        && alt.handle_viewport_input(&data, maho_tui::keys::is_key_release(&data))
    {
        return None;
    }
    let data = mode.filter_terminal_input(&data)?;
    if data.is_empty() {
        return None;
    }
    if maho_tui::keys::matches_key(&data, "shift+ctrl+d") {
        mode.handle_debug_command();
        return None;
    }
    screen.base_mut().apply_input_focus_rules();
    if root_focused(&*screen) {
        // senpi `handleTerminalInput`: key release events reach the focused component only when it
        // opts in with `wantsKeyRelease`.
        if maho_tui::keys::is_key_release(&data) && !root.borrow().wants_key_release() {
            return None;
        }
        return Some(data);
    }
    screen
        .base_mut()
        .handle_terminal_input(&data, maho_tui::keys::is_key_release(&data));
    None
}

/// senpi `handleCtrlZ`: stop the whole process group with `SIGTSTP`. The kernel stops this process
/// before `kill` returns, so returning here means `SIGCONT` (`fg`) resumed it.
#[cfg(unix)]
fn suspend_process() -> Result<(), String> {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(0),
        nix::sys::signal::Signal::SIGTSTP,
    )
    .map_err(|error| error.to_string())
}

#[cfg(not(unix))]
fn suspend_process() -> Result<(), String> {
    Ok(())
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
) -> Result<
    (
        maho_interactive::theme::Theme,
        Vec<maho_interactive::theme::Theme>,
    ),
    String,
> {
    let cwd = session.cwd();
    let agent_dir = session.agent_dir();
    let mut diagnostics: Vec<String> = Vec::new();
    // senpi `loadStartupThemes(settingsManager)`: package-resolved theme resources first, unless
    // `--no-themes` limits discovery to the explicit CLI theme paths (resource-loader.ts:758-760).
    let mut theme_paths: Vec<PathBuf> = Vec::new();
    if !parsed.no_themes {
        let mut settings = startup_theme_settings_manager(session);
        let manager = maho_core::package_manager::DefaultPackageManager::new(
            maho_core::package_manager::PackageManagerOptions {
                cwd: &cwd,
                agent_dir: &agent_dir,
                settings_manager: &mut settings,
            },
        );
        match manager.resolve(None).await {
            Ok(paths) => theme_paths.extend(
                paths
                    .themes
                    .into_iter()
                    .filter(|resource| resource.enabled)
                    .map(|resource| PathBuf::from(resource.path)),
            ),
            Err(error) => diagnostics.push(format!("package themes: {error}")),
        }
    }
    // senpi `resolveCliPaths`: the `--theme` paths resolved against the launch cwd.
    let runtime_config = super::host_runtime::CliRuntimeConfiguration::from_parsed(
        parsed,
        &cwd,
        &agent_dir,
        maho_core::project_trust::AppMode::Interactive,
    );
    let cli_theme_paths = runtime_config.resolved_theme_paths();
    theme_paths.extend(cli_theme_paths.iter().map(PathBuf::from));
    let (registered, mut theme_diagnostics) = super::startup_ui::load_theme_resources(
        theme_paths,
        maho_interactive::theme::detect_color_mode(),
    );
    // senpi resource-loader.ts:765-770: a missing additional (`--theme`) path is an error unless
    // the load already reported that path.
    for path in cli_theme_paths {
        if !std::path::Path::new(&path).exists()
            && !theme_diagnostics
                .iter()
                .any(|diagnostic| diagnostic.path.as_deref() == Some(path.as_str()))
        {
            theme_diagnostics.push(maho_core::diagnostics::ResourceDiagnostic {
                category: None,
                diagnostic_type: maho_core::diagnostics::ResourceDiagnosticType::Error,
                message: "Theme path does not exist".into(),
                path: Some(path),
                collision: None,
            });
        }
    }
    // senpi shows these in the loaded-resources "Theme conflicts" notice (interactive-mode.ts:2580).
    session.set_theme_diagnostics(theme_diagnostics);
    let custom_directory = PathBuf::from(crate::config::get_custom_themes_dir());
    let resolution = super::startup_ui::resolve_startup_theme_with_registered(
        setting,
        std::env::var("COLORFGBG").ok().as_deref(),
        &custom_directory,
        registered.clone(),
    )?;
    // senpi `applyFromSettings` reports a configured theme that fails to load in the TUI
    // (`poll_terminal_theme`), so only the resource diagnostics go to stderr here.
    diagnostics.extend(
        resolution
            .diagnostics
            .into_iter()
            .filter(|diagnostic| resolution.load_error.as_ref() != Some(diagnostic)),
    );
    for diagnostic in &diagnostics {
        maho_core::output_guard::maho_write_stderr(&format!("theme: {diagnostic}\n"));
    }
    Ok((resolution.theme, registered))
}

/// senpi `loadStartupThemes` (startup-ui.ts:65-75):
/// `SettingsManager.inMemory(settingsManager.getGlobalSettings(), { projectTrusted: false })`.
/// The package-resolved theme paths come from the session's **live** global settings (honouring
/// unsaved in-memory overrides), with project settings excluded.
fn startup_theme_settings_manager(
    session: &maho_core::agent_session::AgentSession,
) -> maho_core::settings_manager::SettingsManager {
    maho_core::settings_manager::SettingsManager::in_memory(
        session.with_settings_manager(|settings| settings.get_global().clone()),
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `AgentSession` whose settings manager holds `global` **in memory**
    /// (`SettingsManager::in_memory`), with `agent_dir` on an isolated temp dir. Building the real
    /// session is what makes the regression exercise the production seam instead of a wrapper.
    fn theme_session(
        global: maho_core::settings_manager::Settings,
        agent_dir: &std::path::Path,
    ) -> maho_core::agent_session::AgentSession {
        use maho_core::agent_session::{AgentSession, AgentSessionConfig};
        use std::sync::Arc;
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
            maho_core::model_runtime::CreateModelRuntimeOptions {
                providers: Some(Vec::new()),
                ..Default::default()
            },
        )
        .expect("model runtime");
        let stream_fn: maho_agent::types::StreamFn =
            Arc::new(|_, _, _| maho_ai::types::AssistantMessageEventStream::assistant());
        let cwd = agent_dir.to_string_lossy().into_owned();
        AgentSession::new(AgentSessionConfig {
            agent: maho_agent::Agent::new(maho_agent::AgentOptions {
                stream_fn: Some(stream_fn),
                ..Default::default()
            }),
            session_manager: maho_core::session_manager::SessionManager::in_memory(
                &cwd, None, None,
            ),
            settings_manager: maho_core::settings_manager::SettingsManager::in_memory(
                global, false,
            ),
            cwd: cwd.clone(),
            agent_dir: Some(cwd),
            fallback_now: None,
            retry_random: None,
            scoped_models: Vec::new(),
            favorite_models: Vec::new(),
            flag_values: Default::default(),
            custom_tools: Vec::new(),
            model_runtime: Some(runtime),
            model_registry: None,
            uses_default_stream_function: Some(false),
            initial_active_tool_names: None,
            default_tool_names: None,
            eval_only_tool_names: None,
            allowed_tool_names: None,
            excluded_tool_names: None,
            base_tools_override: None,
            session_start_event: None,
            auto_title_sessions: Some(false),
        })
        .expect("session")
    }

    #[test]
    fn startup_theme_settings_seam_reads_live_global_and_excludes_project() {
        let root = tempfile::tempdir().expect("temp root");
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        // The disk carries a DIFFERENT global with no theme sources; the seam must not read it.
        std::fs::write(agent_dir.join("settings.json"), r#"{"theme":"disk-theme"}"#)
            .expect("disk settings");

        let mut unsaved = maho_core::settings_manager::Settings::new();
        unsaved.insert("theme".to_owned(), serde_json::json!("unsaved-theme"));
        unsaved.insert(
            "themes".to_owned(),
            serde_json::json!([root.path().join("global-themes").to_string_lossy()]),
        );
        let session = theme_session(unsaved.clone(), &agent_dir);

        // The production seam behind `resolve_interactive_startup_theme`: the session's live global
        // settings, with project scope excluded.
        let manager = startup_theme_settings_manager(&session);
        assert_eq!(
            manager.get_global(),
            &unsaved,
            "the seam resolves from the session's live global settings"
        );
        assert!(
            manager.get_global().get("themes").is_some(),
            "the unsaved global theme source is preserved"
        );
        assert!(
            manager.get_project().is_empty(),
            "project theme sources are excluded"
        );
        assert!(
            !manager.is_project_trusted(),
            "project scope is not trusted for startup themes"
        );

        // A disk reload drops the unsaved global, so this fails if the seam reverts to
        // `SettingsManager::create` (it would carry the disk global, which has no theme sources).
        let disk = maho_core::settings_manager::SettingsManager::create(
            &session.cwd(),
            &agent_dir.to_string_lossy(),
            &root.path().to_string_lossy(),
            false,
        );
        assert!(
            disk.get_global().get("themes").is_none(),
            "a disk-create manager cannot see the unsaved global theme source"
        );
    }

    #[test]
    fn r14_pending_native_snapshot_is_not_awaited() {
        let root = tempfile::tempdir().expect("temp root");
        let session = theme_session(maho_core::settings_manager::Settings::new(), root.path());
        // The futures the frame loop passes. A free runner lock is Ready in one poll.
        let free = super::native_snapshots_if_lock_free(
            session.native_tool_renderers_snapshot::<(), serde_json::Value>(),
            session.native_tool_renderers_snapshot::<maho_ext_gpt_apply_patch::preview_format::ApplyPatchRenderState, serde_json::Value>(),
        );
        assert!(
            free.is_some(),
            "a free runner lock still returns both snapshots"
        );

        // Hold the REAL runner lock (the one the old frame loop awaited through these exact
        // futures). The helper must poll once, see Pending, and keep the installed set.
        let guard = super::poll_now(session.hold_extension_runner_lock_for_test())
            .expect("the free runner lock is acquired in one poll");
        let held = super::native_snapshots_if_lock_free(
            session.native_tool_renderers_snapshot::<(), serde_json::Value>(),
            session.native_tool_renderers_snapshot::<maho_ext_gpt_apply_patch::preview_format::ApplyPatchRenderState, serde_json::Value>(),
        );
        assert!(
            held.is_none(),
            "a held runner lock's pending snapshot poll is not awaited"
        );
        drop(guard);
        assert_eq!(super::poll_now(std::future::ready(7)), Some(7));
    }
}
