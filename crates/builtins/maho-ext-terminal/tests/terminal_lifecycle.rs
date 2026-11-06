//! Terminal persistence lifecycle binding (upstream `adoptPersistedTerminalState` +
//! `suspendAndFlushManifest`): the extension acquires the session lease from the host-exposed
//! session dir on a non-reload start, restores the manifest while it owns the lease, and
//! records + releases on shutdown.
use maho_ext_api::*;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

struct Session {
    dir: PathBuf,
    id: &'static str,
}
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str {
        self.id
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> {
        vec![]
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        vec![]
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn get_session_dir(&self) -> Option<PathBuf> {
        Some(self.dir.clone())
    }
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> {
        vec![]
    }
    fn get_available(&self) -> Vec<Model> {
        vec![]
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}
struct Ui;
impl ExtensionUi for Ui {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("unavailable".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
fn context(dir: &Path) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(Ui),
        // Rpc: a long-lived host. A one-shot print/json turn keeps no durable terminal state.
        mode: ExtensionMode::Rpc,
        has_ui: false,
        cwd: dir.into(),
        agent_dir: dir.into(),
        session_manager: Arc::new(Session {
            dir: dir.into(),
            id: "terminal-lifecycle",
        }),
        model_registry: Arc::new(Registry),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: vec![],
        goal_store_file: None,
        loaded_extension_paths: vec![],
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: vec![],
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}
/// Minimal host-side session actions so the extension's `session_start` toolset handler resolves.
/// Production binds these through `AgentSession` before any `session_start` is dispatched; the test
/// api must supply them or the handler's `get_active_tools`/`set_active_tools` calls fail closed.
struct SessionActions;
impl ExtensionSessionActions for SessionActions {
    fn set_session_name(&self, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_session_name(&self) -> Result<Option<String>, ExtensionFailure> {
        Ok(None)
    }
    fn set_label(&self, _: &str, _: Option<&str>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn execute_tool<'a>(
        &'a self,
        name: &'a str,
        _: JsonValue,
        _: ExecuteToolOptions,
    ) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            Err(ExecuteToolError {
                code: ExecuteToolErrorCode::UnknownTool,
                tool_name: name.into(),
                message: "unused".into(),
                active_tools: vec![],
            })
        })
    }
    fn get_active_tools(&self) -> Result<Vec<String>, ExtensionFailure> {
        Ok(vec![])
    }
    fn set_active_tools(&self, _: Vec<String>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn refresh_tools(&self) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_removed_tool_hint(&self, _: &str, _: &str) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn register_lazy_tool_activator(&self, _: LazyToolActivator) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_commands(&self) -> Result<Vec<SlashCommandInfo>, ExtensionFailure> {
        Ok(vec![])
    }
    fn set_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }
    fn get_thinking_level(&self) -> Result<ThinkingLevel, ExtensionFailure> {
        Ok(ThinkingLevel::Medium)
    }
    fn set_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn set_session_model(&self, _: Model) -> ExtensionFuture<'_, bool> {
        Box::pin(async { Ok(false) })
    }
    fn set_session_thinking_level(&self, _: ThinkingLevel) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn set_session_fast_mode(&self, _: bool) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: &'a Path,
        _: ExecOptions,
    ) -> ExtensionFuture<'a, ExecResult> {
        Box::pin(async { Err("unused".into()) })
    }
}
fn api(dir: &Path) -> ExtensionApi {
    let runtime = ExtensionRuntime::default();
    runtime.bind_session_actions(Arc::new(SessionActions));
    let mut api = ExtensionApi::new(
        LoadedExtension::new("terminal", dir.into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        runtime,
    );
    maho_ext_terminal::extension::TerminalExtension.register(&mut api);
    api
}
async fn start(api: &ExtensionApi, ctx: &ExtensionContext, reason: SessionReason) {
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent {
        sizes_only: false,
        reason,
        initial_model_provenance: None,
        previous_session_file: None,
        attended: None,
    });
    for handler in &api.registered.handlers[&EventKind::SessionStart] {
        handler(&mut event, ctx)
            .await
            .expect("session start handler");
    }
}
async fn shutdown(api: &ExtensionApi, ctx: &ExtensionContext, reason: SessionReason) {
    let mut event = ExtensionEvent::SessionShutdown(SessionShutdownEvent {
        reason,
        target_session_file: None,
        signal: None,
    });
    for handler in &api.registered.handlers[&EventKind::SessionShutdown] {
        handler(&mut event, ctx).await.expect("shutdown handler");
    }
}
fn encoded() -> String {
    maho_core::session_sidecar_store::encoded_session_id("terminal-lifecycle")
}
fn state_dir(dir: &Path) -> PathBuf {
    dir.join("extensions/terminal")
}

/// A manifest on disk for session `id` with nothing in it: a start finds something to restore, so
/// it acquires the lease at once instead of waiting for the first durable need.
fn seed_manifest(dir: &Path, id: &str) {
    let encoded = maho_core::session_sidecar_store::encoded_session_id(id);
    std::fs::create_dir_all(state_dir(dir)).expect("create fixture state directory");
    std::fs::write(
        state_dir(dir).join(format!("{encoded}.json")),
        serde_json::json!({"version":1,"sessionId":id,"monitors":[],"backgroundSessions":[],"updatedAt":0})
            .to_string(),
    )
    .expect("seed fixture manifest");
}

/// The registered monitor tool of `api`.
fn monitor_tool(api: &ExtensionApi) -> &maho_tools::definition::ToolDefinition {
    &api.registered
        .tools
        .iter()
        .find(|tool| tool.definition.name == "monitor")
        .expect("monitor tool")
        .definition
}

/// terminal-lazy-persistence.test.ts: a session that never registers anything leaves no terminal
/// state files at all.
#[tokio::test]
async fn a_session_that_registers_nothing_leaves_no_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context_for(dir.path(), "terminal-lazy-nothing");
    let api = api(dir.path());
    start(&api, &ctx, SessionReason::Startup).await;
    assert!(!state_dir(dir.path()).exists(), "no lease at start");
    shutdown(&api, &ctx, SessionReason::Quit).await;
    assert!(!state_dir(dir.path()).exists(), "nothing written at quit");
}

/// monitor.ts:198-215: a create refused by the pure checks (two sources, or wakes_per_hour on a
/// non-command watch) binds no persistence: no lease, no manifest, no state dir.
#[tokio::test]
async fn an_invalid_monitor_create_writes_no_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context_for(dir.path(), "terminal-lazy-invalid");
    let api = api(dir.path());
    start(&api, &ctx, SessionReason::Startup).await;
    for params in [
        serde_json::json!({"description":"both","command":"cat","path":"file","persistent":true}),
        serde_json::json!({"description":"paced rpc","target":"all","persistent":true,"wakes_per_hour":12}),
    ] {
        (monitor_tool(&api).execute)(maho_tools::definition::ToolCall {
            id: "invalid",
            params,
            signal: Default::default(),
            on_update: None,
            context: None,
        })
        .await
        .expect_err("refused before persistence");
        assert!(
            !state_dir(dir.path()).exists(),
            "a refused create binds no persistence"
        );
    }
    shutdown(&api, &ctx, SessionReason::Quit).await;
    assert!(!state_dir(dir.path()).exists());
}

/// terminal-lazy-persistence.test.ts: the first persistent monitor creates the lease and the
/// manifest together.
#[tokio::test]
async fn the_first_persistent_monitor_creates_the_lease_and_the_manifest_together() {
    let id = "terminal-lazy-first";
    let encoded = maho_core::session_sidecar_store::encoded_session_id(id);
    let dir = tempfile::tempdir().unwrap();
    let ctx = context_for(dir.path(), id);
    let api = api(dir.path());
    start(&api, &ctx, SessionReason::Startup).await;
    assert!(!state_dir(dir.path()).exists());
    let result = (monitor_tool(&api).execute)(maho_tools::definition::ToolCall {
        id: "first",
        params: serde_json::json!({"description":"first watch","command":"cat","persistent":true}),
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("persistent monitor starts");
    assert!(result.details.is_some());
    let mut files = std::fs::read_dir(state_dir(dir.path()))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect::<Vec<_>>();
    files.sort();
    let mut expected = vec![
        format!("{encoded}.json"),
        format!("{encoded}.lease"),
        "state".to_owned(),
    ];
    expected.sort();
    assert_eq!(files, expected);
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(state_dir(dir.path()).join(format!("{encoded}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["monitors"][0]["description"], "first watch");
    shutdown(&api, &ctx, SessionReason::Quit).await;
}

#[tokio::test]
async fn session_start_acquires_the_lease_and_shutdown_records_then_releases_it() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let lease = state_dir(dir.path()).join(format!("{}.lease", encoded()));
    let manifest = state_dir(dir.path()).join(format!("{}.json", encoded()));
    seed_manifest(dir.path(), "terminal-lifecycle");
    let api = api(dir.path());
    start(&api, &ctx, SessionReason::Startup).await;
    assert!(
        lease.is_file(),
        "the lease must be acquired from the session dir"
    );
    shutdown(&api, &ctx, SessionReason::Quit).await;
    assert!(!lease.exists(), "shutdown releases the lease");
    assert!(manifest.is_file(), "shutdown records the manifest");
}

#[tokio::test]
async fn a_reload_generation_inherits_the_lease_and_records_and_releases_on_quit() {
    // Its own session id: the lease token map is process-wide and keyed by session.
    let id = "terminal-lifecycle-reload";
    let encoded = maho_core::session_sidecar_store::encoded_session_id(id);
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = context(dir.path());
    ctx.session_manager = Arc::new(Session {
        dir: dir.path().into(),
        id,
    });
    let lease = state_dir(dir.path()).join(format!("{encoded}.lease"));
    let manifest = state_dir(dir.path()).join(format!("{encoded}.json"));
    let owner = api(dir.path());
    start(&owner, &ctx, SessionReason::Startup).await;
    // A pre-reload persistent watch: recorded at once, so the manifest exists before the reload.
    let monitor = owner
        .registered
        .tools
        .iter()
        .find(|tool| tool.definition.name == "monitor")
        .expect("monitor tool");
    let result = (monitor.definition.execute)(maho_tools::definition::ToolCall {
        id: "pre-reload",
        params: serde_json::json!({"description":"pre-reload watch","command":"cat","persistent":true}),
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("persistent monitor starts");
    let monitor_id = result
        .details
        .as_ref()
        .and_then(|details| details["monitor_id"].as_str())
        .expect("monitor id")
        .to_owned();
    assert!(manifest.is_file(), "the registration is recorded at once");
    assert!(lease.is_file(), "the first durable monitor took the lease");
    shutdown(&owner, &ctx, SessionReason::Reload).await;
    assert!(lease.is_file(), "a reload keeps the lease");
    let reloaded = api(dir.path());
    start(&reloaded, &ctx, SessionReason::Reload).await;
    assert!(lease.is_file());
    shutdown(&reloaded, &ctx, SessionReason::Quit).await;
    assert!(
        !lease.exists(),
        "the inherited lease is released by (path, own pid)"
    );
    assert!(
        manifest.is_file(),
        "the reload generation records the manifest"
    );
    // SF-2 seedFromDisk: the reload writer started from the pre-reload manifest, so the watch
    // that survived the reload is still recorded (suspended) after quit.
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).expect("manifest written"))
            .expect("manifest json");
    let monitors = saved["monitors"].as_array().expect("monitors");
    assert_eq!(
        monitors.len(),
        1,
        "the pre-reload watch survives the reload: {saved}"
    );
    assert_eq!(monitors[0]["monitorId"], monitor_id);
    assert_eq!(monitors[0]["suspended"], true);
}

#[tokio::test]
async fn a_context_without_a_session_dir_keeps_no_durable_terminal_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = context(dir.path());
    ctx.session_manager = Arc::new(Session {
        dir: PathBuf::new(),
        id: "terminal-lifecycle",
    });
    let api = api(dir.path());
    start(&api, &ctx, SessionReason::Startup).await;
    shutdown(&api, &ctx, SessionReason::Quit).await;
    assert!(
        !state_dir(dir.path()).exists(),
        "no session dir means no lease and no manifest"
    );
}

#[tokio::test]
async fn a_persistent_monitor_created_through_the_registered_tool_is_recorded_in_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let manifest = state_dir(dir.path()).join(format!("{}.json", encoded()));
    let api = api(dir.path());
    start(&api, &ctx, SessionReason::Startup).await;
    let monitor = api
        .registered
        .tools
        .iter()
        .find(|tool| tool.definition.name == "monitor")
        .expect("monitor tool");
    let result = (monitor.definition.execute)(maho_tools::definition::ToolCall {
        id: "durable",
        params: serde_json::json!({"description":"stdin watch","command":"cat","persistent":true}),
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("persistent monitor starts");
    let monitor_id = result
        .details
        .as_ref()
        .and_then(|details| details["monitor_id"].as_str())
        .expect("monitor id")
        .to_owned();
    shutdown(&api, &ctx, SessionReason::Quit).await;
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).expect("manifest written"))
            .expect("manifest json");
    let monitors = saved["monitors"].as_array().expect("monitors");
    assert_eq!(
        monitors.len(),
        1,
        "the tool-created durable monitor is recorded: {saved}"
    );
    assert_eq!(monitors[0]["monitorId"], monitor_id);
    assert_eq!(monitors[0]["suspended"], true);
}

/// A spec the monitor tool queued while no writer was bound (upstream `handMonitorSpec`) is
/// drained into the writer SessionStart binds, on the fresh-start (`own`) and the reload path,
/// and is in the manifest the next shutdown writes. The binding map is process-wide, so each
/// case uses its own session id.
async fn queued_spec_is_recorded_after_session_start(
    id: &'static str,
    reason: SessionReason,
    dir: tempfile::TempDir,
) {
    use maho_ext_terminal::terminal_manifest_model::{MonitorRegistration, MonitorSpec};
    let mut ctx = context(dir.path());
    ctx.session_manager = Arc::new(Session {
        dir: dir.path().into(),
        id,
    });
    let manifest = state_dir(dir.path()).join(format!(
        "{}.json",
        maho_core::session_sidecar_store::encoded_session_id(id)
    ));
    seed_manifest(dir.path(), id);
    assert!(
        maho_ext_terminal::tools::monitor_manifest_binding::hand_monitor_spec(
            Some(id),
            MonitorRegistration {
                monitor_id: "mon_QUEUEDWHILEUNBND".into(),
                spec: MonitorSpec::Command {
                    description: "queued watch".into(),
                    command: "cat".into(),
                    filter: None,
                    human_input: None,
                    wakes_per_hour: None,
                    cwd: None,
                    persistent: true,
                },
            },
        )
        .is_none(),
        "no writer is bound yet, so the spec is queued"
    );
    let api = api(dir.path());
    start(&api, &ctx, reason).await;
    assert!(
        maho_ext_terminal::tools::monitor_manifest_binding::is_terminal_manifest_writer_bound(id)
    );
    assert_eq!(
        maho_ext_terminal::tools::monitor_manifest_binding::pending_durable_spec_count(id),
        0,
        "the queue drained into the bound writer"
    );
    // The drain records through the bound writer, which persists at once: the entry is on disk
    // before any shutdown (the spec has no live runtime here, so the shutdown sync would drop it).
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest).expect("manifest written"))
            .expect("manifest json");
    let monitors = saved["monitors"].as_array().expect("monitors");
    assert_eq!(monitors.len(), 1, "the queued spec is recorded: {saved}");
    assert_eq!(monitors[0]["monitorId"], "mon_QUEUEDWHILEUNBND");
    shutdown(&api, &ctx, SessionReason::Quit).await;
}

#[tokio::test]
async fn a_spec_queued_while_unbound_is_recorded_after_a_fresh_session_start() {
    queued_spec_is_recorded_after_session_start(
        "terminal-queued-fresh",
        SessionReason::Startup,
        tempfile::tempdir().unwrap(),
    )
    .await;
}

/// The reload bind path: the pre-reload generation holds the lease and detaches on its reload
/// shutdown (upstream `detachPersistence` unbinds), a spec is queued while unbound, and the reload
/// generation that inherits the lease drains it.
#[tokio::test]
async fn a_spec_queued_while_unbound_is_recorded_after_a_reload_session_start() {
    let id = "terminal-queued-reload";
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = context(dir.path());
    ctx.session_manager = Arc::new(Session {
        dir: dir.path().into(),
        id,
    });
    seed_manifest(dir.path(), id);
    let owner = api(dir.path());
    start(&owner, &ctx, SessionReason::Startup).await;
    assert!(
        maho_ext_terminal::tools::monitor_manifest_binding::is_terminal_manifest_writer_bound(id)
    );
    shutdown(&owner, &ctx, SessionReason::Reload).await;
    assert!(
        !maho_ext_terminal::tools::monitor_manifest_binding::is_terminal_manifest_writer_bound(id),
        "a reload shutdown detaches the recorder"
    );
    queued_spec_is_recorded_after_session_start(id, SessionReason::Reload, dir).await;
}

/// terminal-restore-session.test.ts (g): a print-mode (one-shot) start restores nothing and takes
/// no lease; json likewise (upstream `terminalStateDir` skips both).
#[tokio::test]
async fn a_print_or_json_start_takes_no_lease_and_records_no_manifest() {
    for mode in [ExtensionMode::Print, ExtensionMode::Json] {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = context(dir.path());
        ctx.mode = mode;
        let api = api(dir.path());
        start(&api, &ctx, SessionReason::Startup).await;
        assert!(
            !state_dir(dir.path()).exists(),
            "{mode:?}: no lease is taken"
        );
        shutdown(&api, &ctx, SessionReason::Quit).await;
        assert!(
            !state_dir(dir.path()).exists(),
            "{mode:?}: no manifest is recorded"
        );
    }
}

/// The token the lease file records for `id`.
fn lease_token(dir: &Path, id: &str) -> Option<String> {
    let encoded = maho_core::session_sidecar_store::encoded_session_id(id);
    let raw = std::fs::read(state_dir(dir).join(format!("{encoded}.lease"))).ok()?;
    maho_ext_terminal::manifest_lease::read_lease_record(&raw)?.token
}

/// A context for session `id` whose session dir is `dir`.
fn context_for(dir: &Path, id: &'static str) -> ExtensionContext {
    let mut ctx = context(dir);
    ctx.session_manager = Arc::new(Session {
        dir: dir.into(),
        id,
    });
    ctx
}

/// The resumed generation owns the lease under a new token: it acquired (or re-entered) and did
/// not wait on a "held by" holder, which would leave the lease file on the first token.
fn assert_owned_under_a_new_token(dir: &Path, id: &str, first: &str) {
    let encoded = maho_core::session_sidecar_store::encoded_session_id(id);
    let current = maho_ext_terminal::manifest_lease::current_lease_token(&encoded)
        .expect("the resumed generation holds a token");
    assert_ne!(current, first, "the token rotates");
    assert_eq!(lease_token(dir, id).as_deref(), Some(current.as_str()));
}

/// terminal-restore-session.test.ts (h): an in-process /resume rotates the token and owns the
/// lease, never "held by".
#[tokio::test]
async fn an_in_process_resume_rotates_the_token_and_is_never_held_by() {
    let id = "terminal-lifecycle-resume";
    let dir = tempfile::tempdir().unwrap();
    let ctx = context_for(dir.path(), id);
    seed_manifest(dir.path(), id);
    let api = api(dir.path());
    start(&api, &ctx, SessionReason::Startup).await;
    let first = lease_token(dir.path(), id).expect("the fresh start holds a v2 lease");
    shutdown(&api, &ctx, SessionReason::Resume).await;
    start(&api, &ctx, SessionReason::Resume).await;
    assert_owned_under_a_new_token(dir.path(), id, &first);
    shutdown(&api, &ctx, SessionReason::Quit).await;
    assert_eq!(lease_token(dir.path(), id), None, "quit releases the lease");
}

/// terminal-restore-session.test.ts (h2): a lease this process never released (its shutdown cut
/// short, so the stale same-pid record is back on disk) is re-entered, not "held by".
#[tokio::test]
async fn a_same_pid_lease_left_by_a_cut_short_shutdown_is_re_entered() {
    let id = "terminal-lifecycle-reentry";
    let dir = tempfile::tempdir().unwrap();
    let ctx = context_for(dir.path(), id);
    let lease = state_dir(dir.path()).join(format!(
        "{}.lease",
        maho_core::session_sidecar_store::encoded_session_id(id)
    ));
    seed_manifest(dir.path(), id);
    let api = api(dir.path());
    start(&api, &ctx, SessionReason::Startup).await;
    let stale = std::fs::read(&lease).unwrap();
    let first = lease_token(dir.path(), id).expect("the fresh start holds a v2 lease");
    shutdown(&api, &ctx, SessionReason::Resume).await;
    assert!(!lease.exists(), "the resume shutdown released the lease");
    std::fs::write(&lease, stale).unwrap();
    start(&api, &ctx, SessionReason::Resume).await;
    assert_owned_under_a_new_token(dir.path(), id, &first);
    shutdown(&api, &ctx, SessionReason::Quit).await;
    assert!(!lease.exists(), "quit releases the re-entered lease");
}

#[tokio::test]
async fn shutdown_publishes_an_empty_monitor_wake_source_after_a_live_monitor() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let api = api(dir.path());
    let (sender, mut states) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = api.events.on(
        "wake_source_state",
        Arc::new(move |state| {
            if state["source"] == "terminal-monitors" {
                sender
                    .send(state["activeCount"].clone())
                    .expect("monitor state");
            }
        }),
    );
    start(&api, &ctx, SessionReason::Startup).await;
    let monitor = api
        .registered
        .tools
        .iter()
        .find(|tool| tool.definition.name == "monitor")
        .expect("monitor tool");
    (monitor.definition.execute)(maho_tools::definition::ToolCall {
        id: "live",
        params: serde_json::json!({"description":"stdin watch","command":"cat"}),
        signal: Default::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("monitor starts");
    let live = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if states.recv().await.expect("state stream") == 1 {
                return;
            }
        }
    })
    .await;
    assert!(
        live.is_ok(),
        "the live monitor is published as one wake source"
    );
    shutdown(&api, &ctx, SessionReason::Quit).await;
    let mut last = None;
    while let Ok(count) = states.try_recv() {
        last = Some(count);
    }
    assert_eq!(
        last,
        Some(serde_json::json!(0)),
        "shutdown leaves no stale monitor wake count"
    );
}
