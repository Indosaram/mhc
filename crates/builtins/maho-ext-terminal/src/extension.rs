pub use crate::extension_state::monitor_state_payload;
#[cfg(test)]
use crate::extension_state::remove_ended_monitor_state_dir;
use crate::extension_state::{
    Backgrounds, Lifecycle, bind_monitor_events, manifest_now_ms, publish_backgrounds,
    send_completion_notice,
};
use crate::manager::TerminalManager;
use crate::tools::{
    bash_input::{BashInputInput, execute_bash_input},
    bash_output::execute_bash_output_view,
    bash_resize::execute_bash_resize,
    context::{TerminalToolResult, error_result},
};
use maho_ext_api::types::{EventKind, EventResult, Extension, ExtensionApi, ExtensionFailure};
use maho_tools::definition::{ToolContent, ToolDefinition, ToolError, ToolResult};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::sync::{Arc, Mutex};
pub struct TerminalExtension;
fn bind_completion(
    id: String,
    manager: Arc<Mutex<TerminalManager>>,
    backgrounds: Backgrounds,
    sender: Arc<ExtensionApi>,
    delivery: Arc<Mutex<Option<crate::notify::NotificationDelivery>>>,
    notifier: Arc<Mutex<crate::notify::TerminalNotifier>>,
    lifecycle: Lifecycle,
) -> Option<tokio::task::JoinHandle<()>> {
    let handle = {
        let mut manager = manager
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        manager.get(&id)?.completion_handle()
    };
    let mut exit = handle.exit_receiver();
    Some(tokio::spawn(async move {
        loop {
            if exit.borrow_and_update().is_some() {
                break;
            }
            if exit.changed().await.is_err() {
                return;
            }
        }
        backgrounds
            .lock()
            .expect("terminal backgrounds")
            .shift_remove(&id);
        publish_backgrounds(&backgrounds, &sender);
        // Upstream `onBackgroundExit`: the manifest forgets the exited background session.
        if let Some(lifecycle) = lifecycle.lock().await.as_mut() {
            lifecycle
                .writer
                .record_background_exit(&id, manifest_now_ms())
                .await;
        }
        let view = handle.exit_view();
        send_completion_notice(&id, &view, &sender, &delivery, &notifier);
    }))
}
fn tool_result(result: TerminalToolResult) -> Result<ToolResult, ToolError> {
    if result.is_error == Some(true) {
        return Err(ToolError::Message(
            result
                .content
                .into_iter()
                .map(|part| part.text)
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    Ok(ToolResult {
        content: result
            .content
            .into_iter()
            .map(|part| ToolContent::Text {
                text: part.text,
                audience: part.audience.map(|_| "model".to_owned()),
            })
            .collect(),
        details: result.details.map(Value::Object),
        structured_content: None,
        is_error: result.is_error,
    })
}
fn terminal_now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}
/// Upstream `terminalStateDir`: `(sessionDir, join(sessionDir, "extensions/terminal"))`, or `None`
/// when the host exposes no durable session dir. The lease lives in the terminal dir; the
/// manifest writer derives its own `extensions/terminal` base from the session dir.
/// Upstream `spillMonitorOverflow`: only paced watches of a session with a durable state dir get an
/// overflow file; a failing file is reported once per monitor, not once per spilled chunk.
fn spill_monitor_overflow(
    ctx: &maho_ext_api::types::ExtensionContext,
    monitors: &Arc<Mutex<crate::monitor_registry::MonitorRegistry>>,
    warned: &Mutex<std::collections::BTreeSet<String>>,
    id: &str,
    lines: &[String],
) -> Option<crate::monitor_overflow::MonitorSpillResult> {
    let (_, terminal_dir) = terminal_state_dir(ctx)?;
    let monitor_id = {
        let monitors = monitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        monitors.wake_pacing(id)?;
        monitors
            .snapshot()
            .into_iter()
            .find(|entry| entry.id == id)?
            .monitor_id?
    };
    match crate::monitor_overflow_file::append_monitor_overflow(&terminal_dir, &monitor_id, lines) {
        Ok(result) => Some(result),
        Err(error) => {
            if warned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(monitor_id.clone())
            {
                ctx.ui.notify(
                    &format!("Could not write the overflow file of {monitor_id}: {error}"),
                    maho_ext_api::types::NotificationType::Warning,
                );
            }
            None
        }
    }
}
/// Upstream `terminalStateDir`: no durable terminal state for a context without an absolute session
/// dir, or for a one-shot `print`/`json` turn, which has no later generation to restore into.
fn terminal_state_dir(
    ctx: &maho_ext_api::types::ExtensionContext,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let terminal_dir = crate::monitor_state_dir::terminal_state_dir(Some(ctx))?;
    let session_dir = ctx.session_manager.get_session_dir()?;
    Some((session_dir, terminal_dir))
}
/// The per-generation restore digest slot (upstream `PersistenceState.digestSlot`/`generation`):
/// one decided digest per generation, held until a model is bound to hear it.
#[derive(Default)]
struct DigestState {
    generation: u64,
    slot: crate::restore_digest::DigestSlot,
    /// Upstream `settings.notify !== "off"`: the model channel stays silent when off.
    notify_off: bool,
}
type Digests = Arc<Mutex<DigestState>>;
/// Upstream `RESTORE_STATUS_KEY`.
const RESTORE_STATUS_KEY: &str = "terminal-restore";

/// Native restore results as the upstream digest shape `restore_digest` reads.
fn restore_digest_input(
    digest: &crate::restore::RestoreDigest,
    downtime_ms: f64,
) -> crate::restore_digest::RestoreDigestInput {
    crate::restore_digest::RestoreDigestInput {
        store_error: digest.store_error,
        results: digest
            .results
            .iter()
            .map(|result| crate::restore_digest::MonitorRestoreResult {
                monitor_id: result.monitor_id.clone(),
                description: result.description.clone(),
                kind: if result.file { "file" } else { "command" }.to_owned(),
                outcome: result.outcome.to_owned(),
                command: result.command.clone(),
                path: result.path.clone(),
                reason: result.reason.clone(),
                orphan: result.orphan.clone(),
            })
            .collect(),
        // A background session never survives its generation: it is reported, never respawned.
        background_sessions: digest
            .background_sessions
            .iter()
            .map(|session| crate::restore_digest::BackgroundRestoreResult {
                id: session.id.clone(),
                command: session.command.clone(),
                outcome: "exited".to_owned(),
                pid: None,
            })
            .collect(),
        downtime_ms,
    }
}

/// Upstream `flushRestoreDigest`: deliver the pending digest once a model is bound; with
/// `terminal.notify` off only the user is told.
fn flush_restore_digest(
    digests: &Digests,
    sender: &ExtensionApi,
    ctx: &maho_ext_api::types::ExtensionContext,
    has_model: bool,
) {
    let mut state = digests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let notify_off = state.notify_off;
    state.slot.flush(|message| {
        if notify_off {
            ctx.ui.notify(
                &message.content,
                maho_ext_api::types::NotificationType::Info,
            );
            return true;
        }
        let notify = |content: &str| {
            ctx.ui
                .notify(content, maho_ext_api::types::NotificationType::Info)
        };
        crate::restore_digest::deliver_restore_digest(
            has_model,
            ctx.has_ui.then_some(&notify as &dyn Fn(&str)),
            message,
            |custom, options| {
                if let Err(error) = sender.send_message(custom, options) {
                    maho_ext_api::diagnostic(&format!("terminal restore digest failed: {error}"));
                }
            },
        )
    });
}

/// Upstream `sweepTerminalStateDir` after a takeover: stale leases and empty manifests of other
/// sessions; housekeeping only, a failure leaves the files for the next start.
fn sweep_terminal_state(
    terminal_dir: &std::path::Path,
    encoded: &str,
) -> tokio::task::JoinHandle<()> {
    let dir = terminal_dir.to_path_buf();
    let keep: std::collections::HashSet<String> =
        [format!("{encoded}.lease"), format!("{encoded}.json")]
            .into_iter()
            .collect();
    tokio::task::spawn_blocking(move || {
        let options = crate::terminal_state_gc::SweepOptions {
            self_pid: f64::from(std::process::id()),
            keep: &keep,
            limit: None,
            is_process_alive: None,
            read_process_start_ms: None,
        };
        let _ = crate::terminal_state_gc::sweep_terminal_state_dir(&dir, &options);
    })
}
/// What a takeover (an immediate acquire or a lease-keeper handover) needs to restore.
#[derive(Clone)]
struct TakeoverContext {
    state: Lifecycle,
    /// Upstream `state.recordedBackgroundIds`, cleared when `own` installs its writer.
    recorded: crate::extension_state::RecordedBackgroundIds,
    manager: Arc<Mutex<TerminalManager>>,
    monitors: Arc<Mutex<crate::monitor_registry::MonitorRegistry>>,
    digests: Digests,
    terminal_dir: std::path::PathBuf,
    encoded: String,
    shell: Option<String>,
    settings: crate::settings::ResolvedTerminalSettings,
    generation: u64,
    /// Upstream `whenRestoreDecided`: the last generation whose restore was decided.
    decided: Arc<tokio::sync::watch::Sender<u64>>,
    /// Upstream `restoreSessionTestHooks.keeperIntervalMs`; `None` in production.
    keeper_interval_ms: Option<u64>,
    /// Upstream `restoreSessionTestHooks.graceMs`; `None` (RESTORE_GRACE_MS) in production.
    restore_grace_ms: Option<u64>,
    /// Upstream `own` ends with `flushRestoreDigest`: deliver the decided digest once a model is bound.
    flush: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Upstream `state.restoreInFlight`, awaited by shutdown.
    restore_tasks: RestoreTasks,
    /// Set by shutdown: the grace window ends at once.
    grace_stop: Arc<tokio::sync::watch::Sender<bool>>,
    ui: Option<Arc<dyn maho_ext_api::types::ExtensionUi>>,
}
/// Upstream `restoreInFlight`: the detached restore tasks (a takeover's `own`, the grace-window
/// `decide`) that shutdown awaits before it records the manifest.
type RestoreTasks = Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>;
/// Awaits every restore task in flight, including one a task spawned while it was awaited.
async fn await_restores(tasks: &RestoreTasks) {
    loop {
        let next = tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop();
        let Some(task) = next else {
            return;
        };
        let _ = task.await;
    }
}
/// Upstream `stopPersistence` (restore half): the generation is over, so a restore still in its
/// grace window ends it at once (a watch still running counts as restored and stays in the
/// manifest), decides nothing for the slot, and is awaited before the manifest is recorded. Native
/// divergence: upstream awaits the whole grace window instead of cutting it short.
async fn stop_restores(
    digests: &Digests,
    grace_stop: &tokio::sync::watch::Sender<bool>,
    tasks: &RestoreTasks,
) {
    digests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .generation += 1;
    grace_stop.send_replace(true);
    await_restores(tasks).await;
}
type ExitWatch = tokio::sync::watch::Receiver<Option<Result<maho_pty::PtyExit, String>>>;
impl TakeoverContext {
    /// Upstream durable-command `exitsWithin(runtime, graceMs)`: a respawned command watch is
    /// restored only if it is still running when the grace window closes. An exit inside it is
    /// `completed` (code 0) or `lost` with `exited <code> in <ms>ms: <first output line>`, and the
    /// entry is not kept in the manifest (its id is returned). All windows share one deadline
    /// (upstream `Promise.all`).
    async fn settle_restored_commands(
        &self,
        digest: &mut crate::restore::RestoreDigest,
    ) -> Vec<String> {
        let watches: Vec<(usize, String, ExitWatch)> = {
            let mut manager = self
                .manager
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            digest
                .results
                .iter()
                .enumerate()
                .filter(|(_, result)| {
                    !result.file && matches!(result.outcome, "restored" | "muted")
                })
                .filter_map(|(index, result)| {
                    let id = manager.resolve_id(&result.monitor_id)?;
                    let exit = manager.get(&id)?.subscribe_exit();
                    Some((index, id, exit))
                })
                .collect()
        };
        let mut settled_ids = Vec::new();
        if watches.is_empty() {
            return settled_ids;
        }
        let started = std::time::Instant::now();
        let mut stopped = self.grace_stop.subscribe();
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_millis(
                self.restore_grace_ms
                    .unwrap_or(crate::shared::RESTORE_GRACE_MS),
            );
        for (index, id, mut exit) in watches {
            let code = tokio::select! {
                biased;
                settled = tokio::time::timeout_at(deadline, exit.wait_for(Option::is_some)) => match settled {
                    Ok(Ok(settled)) => settled
                        .as_ref()
                        .and_then(|result| result.as_ref().ok())
                        .and_then(|exit| exit.exit_code),
                    _ => continue,
                },
                // Shutdown: every watch not yet seen exiting counts as restored.
                _ = stopped.wait_for(|stop| *stop) => break,
            };
            let result = &mut digest.results[index];
            match result.outcome {
                "muted" => digest.muted -= 1,
                _ => digest.restored -= 1,
            }
            if code == Some(0) {
                result.outcome = "completed";
                digest.completed += 1;
            } else {
                let output = self
                    .manager
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&id)
                    .and_then(|runtime| runtime.full_output().ok())
                    .unwrap_or_default();
                let line = crate::output_format::sanitize_terminal_output(&output)
                    .split('\n')
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(|line| line.chars().take(200).collect::<String>())
                    .unwrap_or_default();
                let exit =
                    code.map_or_else(|| "exited".to_owned(), |code| format!("exited {code}"));
                let elapsed = started.elapsed().as_millis();
                result.outcome = "lost";
                result.reason = Some(if line.is_empty() {
                    format!("{exit} in {elapsed}ms")
                } else {
                    format!("{exit} in {elapsed}ms: {line}")
                });
                digest.lost += 1;
            }
            settled_ids.push(result.monitor_id.clone());
        }
        settled_ids
    }
    /// Upstream `own(lease)`: restore the manifest, set the decided digest, sweep stale state.
    async fn own(&self, mut lifecycle: crate::terminal_state::TerminalStateLifecycle, now: f64) {
        // Upstream `startPersistence`: stale overflow files are swept fire-and-forget; errors are swallowed.
        {
            let dir = self.terminal_dir.clone();
            tokio::spawn(async move {
                let _ =
                    crate::monitor_overflow_file::sweep_monitor_overflow_files_default(&dir).await;
            });
        }
        let restore_state = lifecycle.read_restore_state().await;
        let downtime = match &restore_state {
            Ok(Some(manifest)) => (now - manifest.updated_at).max(0.0),
            _ => 0.0,
        };
        let mut readable_manifest = matches!(restore_state, Ok(Some(_)));
        let reap_result = if let Ok(Some(manifest)) = &restore_state {
            lifecycle.writer.reap_orphans(manifest, now).await
        } else {
            Ok(())
        };
        let digest = match reap_result {
            Ok(()) => {
                let mut manager = self
                    .manager
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut monitors = self
                    .monitors
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                lifecycle.apply_restore(
                    restore_state,
                    &mut manager,
                    &mut monitors,
                    now,
                    self.shell.as_deref(),
                    &self.settings,
                )
            }
            Err(error) => {
                if let Some(ui) = &self.ui {
                    ui.notify(
                        &format!("Terminal restore failed: {error}"),
                        maho_ext_api::types::NotificationType::Warning,
                    );
                }
                maho_ext_api::diagnostic(&format!("Terminal restore failed: {error}"));
                readable_manifest = false;
                crate::restore::RestoreDigest {
                    store_error: true,
                    ..Default::default()
                }
            }
        };
        // Bound before the grace window: monitors created meanwhile reach this recorder. Upstream
        // `bindTerminalManifestWriter` drains the specs queued while unbound; the state lock is
        // held across bind and install, so no monitor call sees the session bound with no
        // lifecycle installed (which would drop its spec).
        let mut slot = self.state.lock().await;
        let session_id = lifecycle.writer.session_id().to_owned();
        crate::tools::monitor_manifest_binding::bind_and_drain(
            &session_id,
            &mut lifecycle.writer,
            terminal_now_ms(),
        )
        .await;
        crate::extension_state::replace_lifecycle(&mut slot, &self.recorded, Some(lifecycle));
        drop(slot);
        // Upstream restores on a detached promise: session_start never waits on a grace window.
        let this = self.clone();
        let task = tokio::spawn(async move {
            this.decide(digest, downtime, readable_manifest).await;
        });
        self.restore_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(task);
    }

    /// The rest of upstream `own(lease)` once the restore ran: settle the grace window, rewrite a
    /// readable manifest, drop dead watches' state dirs, set the decided digest, sweep, signal.
    async fn decide(
        &self,
        mut digest: crate::restore::RestoreDigest,
        downtime: f64,
        readable_manifest: bool,
    ) {
        let settled = self.settle_restored_commands(&mut digest).await;
        // A watch that did not come back never runs again: its baseline dir goes with it (upstream
        // removes these before its generation check).
        for result in digest
            .results
            .iter()
            .filter(|result| matches!(result.outcome, "lost" | "expired" | "completed"))
        {
            let _ = crate::monitor_state_dir::remove_monitor_state_dir(
                &self.terminal_dir,
                &result.monitor_id,
            )
            .await;
        }
        // Upstream `if (generation !== state.generation) return;`: a restore that a newer
        // generation overtook touches neither that generation's manifest nor its digest slot, and
        // sweeps, flushes and signals nothing.
        if self
            .digests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
            != self.generation
        {
            return;
        }
        if let Some(lifecycle) = self.state.lock().await.as_mut() {
            for monitor_id in &settled {
                lifecycle.writer.forget_restored(monitor_id);
            }
            // Upstream `persistRestored`: a readable manifest is rewritten to the restore's truth,
            // so a watch that did not come back is gone from disk and never re-run.
            if readable_manifest {
                lifecycle.writer.persist_restored(terminal_now_ms()).await;
            }
        }
        let mut digests = self
            .digests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if digests.generation == self.generation
            && (digest.store_error
                || !digest.results.is_empty()
                || !digest.background_sessions.is_empty())
        {
            digests
                .slot
                .set(crate::restore_digest::build_restore_digest(
                    &restore_digest_input(&digest, downtime),
                    self.generation,
                    crate::restore_digest::RestoreDigestOutcome::Decided,
                    None,
                ));
        }
        drop(digests);
        drop(sweep_terminal_state(&self.terminal_dir, &self.encoded));
        if let Some(flush) = &self.flush {
            flush();
        }
        self.decided.send_replace(self.generation);
    }
}

/// Upstream `wait(holder)`: a deferred digest and a lease keeper that hands the session over
/// once the holder is gone.
fn wait_for_lease(
    ctx: &maho_ext_api::types::ExtensionContext,
    context: &TakeoverContext,
    keeper: &Arc<Mutex<Option<crate::lease_keeper::LeaseKeeper>>>,
    holder: Option<crate::manifest_lease::LeaseRecord>,
    session_dir: std::path::PathBuf,
    session_id: String,
) {
    let holder_pid = holder.as_ref().map(|holder| holder.pid as u32);
    let status = holder_pid.map_or_else(
        || "monitors waiting for the session lease".to_owned(),
        |pid| format!("monitors held by pid {pid}"),
    );
    ctx.ui.set_status(RESTORE_STATUS_KEY, Some(status.as_str()));
    context
        .digests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .slot
        .set(crate::restore_digest::build_restore_digest(
            &crate::restore_digest::RestoreDigestInput::default(),
            context.generation,
            crate::restore_digest::RestoreDigestOutcome::Deferred,
            holder_pid,
        ));
    let takeover = context.clone();
    let ui = ctx.ui.clone();
    let error_ui = ctx.ui.clone();
    let next = crate::lease_keeper::create_lease_keeper(crate::lease_keeper::LeaseKeeperOptions {
        dir: context.terminal_dir.clone(),
        encoded_session_id: context.encoded.clone(),
        on_takeover: Box::new(move |lease| {
            ui.set_status(RESTORE_STATUS_KEY, None);
            let takeover = takeover.clone();
            let mut lifecycle =
                crate::terminal_state::TerminalStateLifecycle::new(&session_dir, &session_id);
            lifecycle.adopt_lease(lease);
            let tasks = takeover.restore_tasks.clone();
            let task = tokio::spawn(async move {
                takeover.own(lifecycle, terminal_now_ms()).await;
            });
            tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(task);
        }),
        on_error: Some(Box::new(move |error| {
            error_ui.notify(
                &format!("Terminal lease check failed, still waiting: {error}"),
                maho_ext_api::types::NotificationType::Warning,
            );
        })),
        interval_ms: context.keeper_interval_ms,
        self_pid: f64::from(std::process::id()),
        self_started_at_ms: crate::process_identity::own_process_started_at_ms(None),
        is_process_alive: None,
        read_process_start_ms: None,
    });
    next.start(holder);
    if let Some(previous) = keeper
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replace(next)
    {
        previous.stop();
    }
}

/// Upstream `startPersistence` `acquire()`: own the lease and restore, or wait on its holder.
async fn acquire_or_wait(
    ctx: &maho_ext_api::types::ExtensionContext,
    context: &TakeoverContext,
    keeper: &Arc<Mutex<Option<crate::lease_keeper::LeaseKeeper>>>,
    mut lifecycle: crate::terminal_state::TerminalStateLifecycle,
    session_dir: std::path::PathBuf,
    session_id: String,
    now: f64,
) {
    match lifecycle.acquire(
        &context.terminal_dir,
        &context.encoded,
        f64::from(std::process::id()),
        now,
    ) {
        Ok(crate::terminal_state::TerminalStateAdoption::Acquired) => {
            ctx.ui.set_status(RESTORE_STATUS_KEY, None);
            context.own(lifecycle, now).await;
        }
        Ok(crate::terminal_state::TerminalStateAdoption::AttachedElsewhere { pid }) => {
            wait_for_lease(
                ctx,
                context,
                keeper,
                Some(crate::manifest_lease::LeaseRecord::v1(pid, 0.0)),
                session_dir,
                session_id,
            );
        }
        Err(error) => {
            ctx.ui.notify(
                &format!("Terminal lease not acquired yet, retrying: {error}"),
                maho_ext_api::types::NotificationType::Warning,
            );
            wait_for_lease(ctx, context, keeper, None, session_dir, session_id);
        }
    }
}

/// The monitor tool's rpc branch (monitor.ts `source === "rpc"` + monitor-rpc.ts
/// `createRpcMonitor`): refuse `human_input`, cap a durable create, arm the watch through the
/// registry's rpc source with no std lock held across the arm, bind its `mon_` id and hand its
/// spec to the session's writer (or queue it until one binds).
async fn create_rpc_monitor_call(
    input: &Value,
    settings: &crate::settings::ResolvedTerminalSettings,
    session_key: Option<String>,
    manager: &Arc<Mutex<TerminalManager>>,
    monitors: &Arc<Mutex<crate::monitor_registry::MonitorRegistry>>,
    lifecycle: Option<&mut crate::terminal_state::TerminalStateLifecycle>,
) -> TerminalToolResult {
    if let Some(refused) = crate::tools::monitor_rpc::precheck_create(
        input,
        crate::tools::monitor_rpc::MonitorSource::Rpc,
    ) {
        return refused;
    }
    if input.get("persistent").and_then(Value::as_bool) == Some(true)
        && let Some(refused) = crate::tools::monitor_manifest_binding::durable_admission_error(
            settings.max_durable_monitors,
            session_key.as_deref(),
            lifecycle.as_deref().map(|lifecycle| &lifecycle.writer),
        )
    {
        return refused;
    }
    let source = monitors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .rpc_source();
    let (result, created) = crate::tools::monitor_rpc::create_rpc_monitor(
        crate::tools::monitor_rpc::RpcMonitorContext {
            source,
            session_id: session_key.clone(),
            agent_dir: maho_core::config::get_agent_dir().into(),
            endpoints: None,
            opener: None,
            now_ms: manifest_now_ms(),
        },
        input,
    )
    .await;
    let Some(created) = created else {
        return result;
    };
    manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .bind_monitor_id(&created.monitor_id, &created.runtime_id);
    match lifecycle {
        Some(lifecycle) => {
            lifecycle
                .writer
                .record_register_with(created.registration, manifest_now_ms())
                .await;
        }
        None => {
            if crate::tools::monitor_manifest_binding::hand_monitor_spec(
                session_key.as_deref(),
                created.registration.registration,
            )
            .is_some()
            {
                maho_ext_api::diagnostic(
                    "terminal manifest: a writer is bound for this session but not held by this generation; the monitor spec was not recorded",
                );
            }
        }
    }
    result
}

/// Upstream `stopPersistence` (first half): stop waiting; a tick in flight either releases what it
/// acquired or finishes its takeover before the caller flushes the manifest.
async fn stop_waiting(keeper: &Arc<Mutex<Option<crate::lease_keeper::LeaseKeeper>>>) {
    let waiting = keeper
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(waiting) = waiting {
        waiting.stop();
        waiting.settled().await;
    }
}

impl Extension for TerminalExtension {
    fn register(&self, api: &mut ExtensionApi) {
        // The session cwd the child placement reads, set by the first session-start handler.
        let session_cwd: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::new(Mutex::new(None));
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let backgrounds: Backgrounds = Arc::new(Mutex::new(indexmap::IndexMap::new()));
        let configuration = Arc::new(Mutex::new((
            crate::settings::TERMINAL_SETTINGS_DEFAULTS,
            None::<String>,
        )));
        let start_configuration = configuration.clone();
        let configuration_manager = manager.clone();
        let start_cwd = session_cwd.clone();
        // Upstream `state.ctx`, read by the monitor-ended sink at call time.
        let session_ctx: Arc<Mutex<Option<maho_ext_api::types::ExtensionContext>>> = Arc::default();
        let start_ctx = session_ctx.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let configuration = start_configuration.clone();
                let manager = configuration_manager.clone();
                let context_cwd = start_cwd.clone();
                let session_ctx = start_ctx.clone();
                Box::pin(async move {
                    *context_cwd
                        .lock()
                        .map_err(|_| ExtensionFailure::new("terminal context cwd poisoned"))? =
                        Some(ctx.cwd.clone());
                    *session_ctx
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ctx.clone());
                    let loaded = crate::settings::load_session_settings(ctx)?;
                    manager
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .configure(&loaded.0);
                    *configuration.lock().map_err(|_| {
                        ExtensionFailure::new("terminal configuration state poisoned")
                    })? = loaded;
                    Ok(EventResult::None)
                })
            }),
        );
        let completion_delivery = Arc::new(Mutex::new(None));
        let terminal_notifier = Arc::new(Mutex::new(crate::notify::TerminalNotifier::default()));
        let notifier: Arc<Mutex<Option<crate::monitor_notify::MonitorNotifier>>> =
            Arc::new(Mutex::new(None));
        let sender = Arc::new(ExtensionApi::new(
            api.registered.clone(),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        let state_lifecycle: Lifecycle = Arc::new(tokio::sync::Mutex::new(None));
        let recorded_background_ids: crate::extension_state::RecordedBackgroundIds = Arc::default();
        // Upstream `state.ensurePersistence`: set by a start with nothing to restore.
        let ensure_persistence: crate::extension_state::EnsurePersistence = Arc::default();
        // Upstream `TerminalExtensionState`: the sinks and the tool context read it at call time.
        let terminal_state = crate::extension_state::TerminalExtensionState {
            sender: sender.clone(),
            ctx: session_ctx.clone(),
            configuration: configuration.clone(),
            session_cwd,
            backgrounds: backgrounds.clone(),
            lifecycle: state_lifecycle.clone(),
            monitor_notifier: notifier.clone(),
            notifier: terminal_notifier.clone(),
            delivery: completion_delivery.clone(),
            recorded_background_ids: recorded_background_ids.clone(),
            ensure_persistence: ensure_persistence.clone(),
        };
        let sinks = crate::extension_state::bundle_sinks(&terminal_state);
        let (on_monitor_event, on_monitor_ended) = (
            sinks.on_monitor_event.clone(),
            sinks.on_monitor_ended.clone(),
        );
        let monitors = Arc::new(Mutex::new(
            crate::monitor_registry::MonitorRegistry::new_with_ended(
                move |event| on_monitor_event(event),
                move |event| on_monitor_ended(event),
            ),
        ));
        let bundle = Arc::new(Mutex::new(
            crate::session_bundle::TerminalSessionBundle::from_parts(
                manager.clone(),
                monitors.clone(),
                backgrounds.clone(),
            ),
        ));
        let reload_bundle = bundle.clone();
        let reload_configuration = configuration.clone();
        api.on(EventKind::SessionStart,Arc::new(move |event,ctx| {let bundle=reload_bundle.clone();let configuration=reload_configuration.clone();Box::pin(async move {
            let previous=crate::extension_state::session_key_of(Some(ctx)).and_then(|key|crate::session_bundle::claim_parked_bundle(&key));
            if let Some(mut previous)=previous {
                if matches!(event,maho_ext_api::types::ExtensionEvent::SessionStart(event) if event.reason==maho_ext_api::types::SessionReason::Reload) {
                    let live=bundle.lock().map_err(|_|ExtensionFailure::new("terminal bundle state poisoned"))?;live.adopt(&previous);live.manager.lock().unwrap_or_else(std::sync::PoisonError::into_inner).configure(&configuration.lock().map_err(|_|ExtensionFailure::new("terminal configuration state poisoned"))?.0);
                }else {previous.teardown().map_err(|error|ExtensionFailure::new(error.to_string()))?;}
            }
            Ok(EventResult::None)
        })}));
        let status_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(None));
        for kind in [EventKind::SessionParked, EventKind::SessionResumed] {
            let monitors = monitors.clone();
            let status = status_task.clone();
            api.on(
                kind,
                Arc::new(move |_, ctx| {
                    let monitors = monitors.clone();
                    let status = status.clone();
                    Box::pin(async move {
                        let monitors = monitors
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if kind == EventKind::SessionParked {
                            monitors.park();
                        } else {
                            monitors.unpark();
                        }
                        let mut status = status
                            .lock()
                            .map_err(|_| ExtensionFailure::new("monitor status state poisoned"))?;
                        if let Some(task) = status.take() {
                            task.abort();
                        }
                        if kind == EventKind::SessionResumed {
                            let ui = ctx.ui.clone();
                            let session = ctx.session_manager.clone();
                            let tui = matches!(ctx.mode, maho_ext_api::types::ExtensionMode::Tui);
                            *status = Some(crate::monitor_status_ticker::bind_monitor_status(
                                monitors.subscribe_state(),
                                move |text| {
                                    if session
                                        .extension_context_actions()
                                        .is_some_and(|actions| actions.assert_active().is_err())
                                    {
                                        return;
                                    }
                                    ui.set_status(
                                        crate::monitor_status::MONITOR_STATUS_KEY,
                                        crate::monitor_status::render_monitor_status(
                                            if tui { "tui" } else { "plain" },
                                            &ui.theme(),
                                            text.as_deref(),
                                        )
                                        .as_deref(),
                                    );
                                },
                            ));
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }
        let status_monitors = monitors.clone();
        let start_status = status_task.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let monitors = status_monitors.clone();
                let status = start_status.clone();
                Box::pin(async move {
                    let mut status = status
                        .lock()
                        .map_err(|_| ExtensionFailure::new("monitor status state poisoned"))?;
                    if let Some(task) = status.take() {
                        task.abort();
                    }
                    let receiver = monitors
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .subscribe_state();
                    let ui = ctx.ui.clone();
                    let tui = matches!(ctx.mode, maho_ext_api::types::ExtensionMode::Tui);
                    let session = ctx.session_manager.clone();
                    *status = Some(crate::monitor_status_ticker::bind_monitor_status(
                        receiver,
                        move |text| {
                            if session
                                .extension_context_actions()
                                .is_some_and(|actions| actions.assert_active().is_err())
                            {
                                return;
                            }
                            ui.set_status(
                                crate::monitor_status::MONITOR_STATUS_KEY,
                                crate::monitor_status::render_monitor_status(
                                    if tui { "tui" } else { "plain" },
                                    &ui.theme(),
                                    text.as_deref(),
                                )
                                .as_deref(),
                            );
                        },
                    ));
                    Ok(EventResult::None)
                })
            }),
        );
        let digests: Digests = Arc::new(Mutex::new(DigestState::default()));
        let lease_keeper: Arc<Mutex<Option<crate::lease_keeper::LeaseKeeper>>> =
            Arc::new(Mutex::new(None));
        let telemetry_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(None));
        let telemetry_monitors = monitors.clone();
        let telemetry_sender = sender.clone();
        let start_telemetry = telemetry_task.clone();
        let telemetry_lifecycle = state_lifecycle.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, _| {
                let monitors = telemetry_monitors.clone();
                let sender = telemetry_sender.clone();
                let task = start_telemetry.clone();
                let lifecycle = telemetry_lifecycle.clone();
                Box::pin(async move {
                    let mut task = task
                        .lock()
                        .map_err(|_| ExtensionFailure::new("monitor telemetry state poisoned"))?;
                    if let Some(previous) = task.take() {
                        previous.abort();
                    }
                    *task = Some(bind_monitor_events(
                        monitors
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .subscribe_state(),
                        sender,
                        lifecycle,
                    ));
                    Ok(EventResult::None)
                })
            }),
        );
        let stepped_aside = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notice_shown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for kind in [EventKind::SessionStart, EventKind::ModelSelect] {
            let sender = sender.clone();
            let stepped_aside = stepped_aside.clone();
            let notice_shown = notice_shown.clone();
            let completion_delivery = completion_delivery.clone();
            let configuration = configuration.clone();
            api.on(
                kind,
                Arc::new(move |event, ctx| {
                    let sender = sender.clone();
                    let stepped_aside = stepped_aside.clone();
                    let notice_shown = notice_shown.clone();
                    let completion_delivery = completion_delivery.clone();
                    let configuration = configuration.clone();
                    Box::pin(async move {
                        let model = match event {
                            maho_ext_api::types::ExtensionEvent::ModelSelect(event) => {
                                Some(&event.model)
                            }
                            _ => ctx.model.as_ref(),
                        };
                        let enabled = std::env::var("PI_ANTHROPIC_BASH").is_ok_and(|value| {
                            matches!(
                                value.trim().to_lowercase().as_str(),
                                "1" | "true" | "yes" | "on"
                            )
                        });
                        let step_aside = enabled
                            && model.is_some_and(|model| {
                                serde_json::to_value(&model.api)
                                    .ok()
                                    .and_then(|value| value.as_str().map(str::to_owned))
                                    .as_deref()
                                    == Some("anthropic-messages")
                            });
                        stepped_aside.store(step_aside, std::sync::atomic::Ordering::SeqCst);
                        let mut active = sender.get_active_tools()?;
                        if !step_aside && !active.iter().any(|tool| tool == "bash") {
                            active.push("bash".to_owned());
                        }
                        for companion in crate::shared::TERMINAL_COMPANION_TOOLS {
                            if !active.iter().any(|tool| tool == companion) {
                                active.push((*companion).to_owned());
                            }
                        }
                        if step_aside
                            && !notice_shown.swap(true, std::sync::atomic::Ordering::SeqCst)
                        {
                            ctx.ui.notify(
                                "native Anthropic bash active: monitor sessions remain available",
                                maho_ext_api::types::NotificationType::Info,
                            );
                        }
                        if !step_aside {
                            notice_shown.store(false, std::sync::atomic::Ordering::SeqCst);
                        }
                        sender.set_active_tools(active)?;
                        *completion_delivery.lock().map_err(|_| {
                            ExtensionFailure::new("terminal delivery state poisoned")
                        })? = crate::notify::get_terminal_notification_delivery(
                            configuration
                                .lock()
                                .map_err(|_| {
                                    ExtensionFailure::new("terminal configuration state poisoned")
                                })?
                                .0
                                .notify,
                            Some(match ctx.mode {
                                maho_ext_api::types::ExtensionMode::Print => "print",
                                maho_ext_api::types::ExtensionMode::Json => "json",
                                _ => "interactive",
                            }),
                            model.is_some(),
                            false,
                        );
                        Ok(EventResult::None)
                    })
                }),
            );
        }
        let prompt_sender = sender.clone();
        api.on(
            EventKind::BeforeAgentStart,
            Arc::new(move |event, _| {
                let sender = prompt_sender.clone();
                let stepped_aside = stepped_aside.clone();
                Box::pin(async move {
                    if stepped_aside.load(std::sync::atomic::Ordering::SeqCst) {
                        return Ok(EventResult::None);
                    }
                    let maho_ext_api::types::ExtensionEvent::BeforeAgentStart(event) = event else {
                        return Ok(EventResult::None);
                    };
                    let eval_only = sender.has_tool("eval")?;
                    Ok(EventResult::BeforeAgentStart(
                        maho_ext_api::types::BeforeAgentStartEventResult {
                            system_prompt: Some(format!(
                                "{}\n{}",
                                event.system_prompt,
                                crate::prompt::build_terminal_prompt_section(eval_only)
                            )),
                            ..Default::default()
                        },
                    ))
                })
            }),
        );
        // Terminal persistence lifecycle: upstream `adoptPersistedTerminalState` on a non-reload
        // start (acquire the lease, restore the manifest, report ONE coalesced digest) and
        // `suspendAndFlushManifest` + lease release on shutdown. The lease and the manifest both
        // live under the session dir the host exposes through `SessionManager::get_session_dir`;
        // a context without one keeps no durable terminal state, like upstream's SDK/in-memory
        // sessions. A reload generation keeps the lease: the same pid still owns it.
        let start_lifecycle = state_lifecycle.clone();
        let start_recorded = recorded_background_ids.clone();
        let lifecycle_manager = manager.clone();
        let lifecycle_monitors = monitors.clone();
        let lifecycle_settings = configuration.clone();
        let lifecycle_sender = sender.clone();
        let lifecycle_digests = digests.clone();
        let lifecycle_keeper = lease_keeper.clone();
        let lifecycle_decided = Arc::new(tokio::sync::watch::channel(0u64).0);
        let restore_tasks: RestoreTasks = Arc::default();
        // The current generation's grace-window stop signal; each SessionStart installs a fresh one.
        let grace_stop: Arc<Mutex<Arc<tokio::sync::watch::Sender<bool>>>> =
            Arc::new(Mutex::new(Arc::new(tokio::sync::watch::channel(false).0)));
        let start_restore_tasks = restore_tasks.clone();
        let start_grace_stop = grace_stop.clone();
        let start_ensure = ensure_persistence.clone();
        api.on(EventKind::SessionStart,Arc::new(move |event,ctx| {let ensure=start_ensure.clone();let state=start_lifecycle.clone();let manager=lifecycle_manager.clone();let monitors=lifecycle_monitors.clone();let configuration=lifecycle_settings.clone();let sender=lifecycle_sender.clone();let digests=lifecycle_digests.clone();let keeper=lifecycle_keeper.clone();let decided=lifecycle_decided.clone();let restore_tasks=start_restore_tasks.clone();let grace_stop=start_grace_stop.clone();let recorded=start_recorded.clone();Box::pin(async move {
            *ensure.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=None;
            let Some((session_dir,terminal_dir))=terminal_state_dir(ctx) else {return Ok(EventResult::None);};
            let session_id=ctx.session_manager.session_id().to_owned();
            let encoded=maho_core::session_sidecar_store::encoded_session_id(&session_id);
            let reload=matches!(event,maho_ext_api::types::ExtensionEvent::SessionStart(event) if event.reason==maho_ext_api::types::SessionReason::Reload);
            let now=terminal_now_ms();
            // A reload generation inherits the lease (same pid) and only rebinds the recorder so
            // manifest coverage continues; a fresh generation acquires the lease and restores.
            let mut lifecycle=crate::terminal_state::TerminalStateLifecycle::new(&session_dir,&session_id);
            // Upstream `holdsLease`: only a reload whose process still holds this session's lease
            // token keeps it; any other reload acquires (or waits) like a fresh start.
            if reload && crate::manifest_lease::current_lease_token(&encoded).is_some() {
                // Bound at once: the specs queued before this generation's writer drain into it.
                // The state lock is held across bind and install, so a monitor call cannot see the
                // session bound while no lifecycle is installed (its spec would be dropped).
                // SF-2: seeded from disk first, so the first post-reload write keeps the pre-reload
                // entries instead of rewriting the manifest from an empty writer.
                lifecycle.writer.seed_from_disk().await;
                let mut slot=state.lock().await;
                crate::tools::monitor_manifest_binding::bind_and_drain(&session_id,&mut lifecycle.writer,now).await;
                crate::extension_state::replace_lifecycle(&mut slot,&recorded,Some(lifecycle));
                return Ok(EventResult::None);
            }
            let (settings,shell)=configuration.lock().map_err(|_|ExtensionFailure::new("terminal configuration state poisoned"))?.clone();
            // Upstream `startPersistence`: a new generation gets a fresh digest slot.
            let generation={let mut digests=digests.lock().unwrap_or_else(std::sync::PoisonError::into_inner);digests.generation+=1;digests.slot=crate::restore_digest::create_digest_slot();digests.notify_off=settings.notify==crate::settings::NotifyMode::Off;digests.generation};
            let context=TakeoverContext {state:state.clone(),recorded,manager:manager.clone(),monitors:monitors.clone(),digests:digests.clone(),terminal_dir:terminal_dir.clone(),encoded:encoded.clone(),shell,settings,generation,decided,keeper_interval_ms:None,restore_grace_ms:None,restore_tasks,grace_stop:{let fresh=Arc::new(tokio::sync::watch::channel(false).0);*grace_stop.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=fresh.clone();fresh},flush:Some({let digests=digests.clone();let sender=sender.clone();let ctx=ctx.clone();Arc::new(move ||flush_restore_digest(&digests,&sender,&ctx,ctx.model.is_some()))}),ui:Some(ctx.ui.clone())};
            // Upstream `startPersistence`: nothing on disk to restore means the lease and the
            // recorder wait for the first durable need (`ensurePersistence`); the restore is
            // decided at once.
            if !std::path::Path::new(lifecycle.writer.store.file_path()).exists() {
                context.decided.send_replace(generation);
                let (ctx,keeper)=(ctx.clone(),keeper.clone());
                *ensure.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(crate::extension_state::LazyPersistence::new(move || {
                    let (ctx,context,keeper,session_dir,session_id)=(ctx.clone(),context.clone(),keeper.clone(),session_dir.clone(),session_id.clone());
                    Box::pin(async move {
                        let lifecycle=crate::terminal_state::TerminalStateLifecycle::new(&session_dir,&session_id);
                        acquire_or_wait(&ctx,&context,&keeper,lifecycle,session_dir,session_id,terminal_now_ms()).await;
                    })
                }));
                return Ok(EventResult::None);
            }
            acquire_or_wait(ctx,&context,&keeper,lifecycle,session_dir,session_id,now).await;
            flush_restore_digest(&digests,&sender,ctx,ctx.model.is_some());
            Ok(EventResult::None)
        })}));
        let shutdown_lifecycle = state_lifecycle.clone();
        let shutdown_monitors = monitors.clone();
        let shutdown_keeper = lease_keeper.clone();
        let shutdown_digests = digests.clone();
        let shutdown_recorded = recorded_background_ids.clone();
        let shutdown_ensure = ensure_persistence.clone();
        api.on(EventKind::SessionShutdown,Arc::new(move |event,ctx| {*shutdown_ensure.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=None;let state=shutdown_lifecycle.clone();let recorded=shutdown_recorded.clone();let monitors=shutdown_monitors.clone();let keeper=shutdown_keeper.clone();let digests=shutdown_digests.clone();let restore_tasks=restore_tasks.clone();let grace_stop=grace_stop.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();Box::pin(async move {
            if matches!(event,maho_ext_api::types::ExtensionEvent::SessionShutdown(event) if event.reason==maho_ext_api::types::SessionReason::Reload) {
                // Upstream `detachPersistence`: the recorder detaches without writing and the
                // session unbinds, so specs created until the next generation binds are queued for
                // it. The lease is deliberately kept: the same pid holds it across the reload.
                stop_waiting(&keeper).await;
                stop_restores(&digests,&grace_stop,&restore_tasks).await;
                let mut slot=state.lock().await;
                if let Some(lifecycle)=crate::extension_state::replace_lifecycle(&mut slot,&recorded,None) {crate::tools::monitor_manifest_binding::unbind_terminal_manifest_writer(lifecycle.writer.session_id());}
                return Ok(EventResult::None);
            }
            // Upstream `stopPersistence`: stop waiting; a tick in flight either releases what it
            // acquired or finishes its takeover before the manifest is flushed.
            stop_waiting(&keeper).await;
            // Upstream `retireLeaseToken(lease.token)` runs BEFORE `await state.restoreInFlight`:
            // this process's token for the session (acquired, taken over, or inherited across a
            // reload) stops counting as a live same-pid holder before the shutdown waits.
            if let Some(token)=crate::manifest_lease::current_lease_token(&maho_core::session_sidecar_store::encoded_session_id(ctx.session_manager.session_id())) {crate::manifest_lease::retire_lease_token(&token);}
            // Upstream `await state.restoreInFlight`: the restore (a takeover's `own` and its grace
            // window) finishes before the final sync, the shutdown record and the release.
            stop_restores(&digests,&grace_stop,&restore_tasks).await;
            // Unbound under the state lock, like the bind: a monitor call never sees the session
            // bound with no lifecycle installed.
            let mut slot=state.lock().await;
            let Some(mut lifecycle)=crate::extension_state::replace_lifecycle(&mut slot,&recorded,None) else {return Ok(EventResult::None);};
            crate::tools::monitor_manifest_binding::unbind_terminal_manifest_writer(lifecycle.writer.session_id());
            drop(slot);
            // Idempotent: the lease this lifecycle holds is the token retired above.
            lifecycle.retire_token();
            // Upstream `suspendAndFlushManifest`: a final live-state sync so the shutdown write
            // carries each watch's burned fire window and mute state.
            let snapshot=monitors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).snapshot();
            if let Err(error)=lifecycle.writer.observe_monitor_state(&snapshot,terminal_now_ms()).await {maho_ext_api::diagnostic(&format!("terminal manifest monitor sync failed: {error}"));}
            lifecycle.record_shutdown(terminal_now_ms()).await;
            if lifecycle.is_owner() {
                if let Err(error)=lifecycle.release() {maho_ext_api::diagnostic(&format!("terminal lease release failed: {error}"));}
            } else if let Some((_session_dir,terminal_dir))=terminal_state_dir(ctx) {
                // A reload generation inherited the lease instead of acquiring it, so it releases
                // by (path, own pid) - which removes exactly that file and no foreign holder's.
                let encoded=maho_core::session_sidecar_store::encoded_session_id(ctx.session_manager.session_id());
                if let Err(error)=crate::terminal_state::release_lease_at(&terminal_dir,&encoded,f64::from(std::process::id())) {maho_ext_api::diagnostic(&format!("terminal lease release failed: {error}"));}
            }
            Ok(EventResult::None)
        })}));
        // Upstream `model_select`: a digest decided before a model was bound is delivered now.
        let select_digests = digests.clone();
        let select_sender = sender.clone();
        api.on(
            EventKind::ModelSelect,
            Arc::new(move |_, ctx| {
                let digests = select_digests.clone();
                let sender = select_sender.clone();
                Box::pin(async move {
                    flush_restore_digest(&digests, &sender, ctx, true);
                    Ok(EventResult::None)
                })
            }),
        );
        let bash_sender = sender.clone();
        let shutdown_sender = sender.clone();
        let lifecycle_delivery = completion_delivery.clone();
        let lifecycle_notifier = notifier.clone();
        let lifecycle_monitors = monitors.clone();
        let lifecycle_configuration = configuration.clone();
        let start_sender = sender.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let notifier = lifecycle_notifier.clone();
                let monitors = lifecycle_monitors.clone();
                let sender = start_sender.clone();
                let lifecycle_delivery = lifecycle_delivery.clone();
                let configuration = lifecycle_configuration.clone();
                Box::pin(async move {
                    use maho_ext_api::types::{
                        CustomMessage, DeliverAs, ExtensionMode, SendMessageOptions,
                    };
                    let settings = configuration
                        .lock()
                        .map_err(|_| {
                            ExtensionFailure::new("terminal configuration state poisoned")
                        })?
                        .0
                        .clone();
                    let delivery_mode = crate::notify::get_terminal_notification_delivery(
                        settings.notify,
                        Some(match ctx.mode {
                            ExtensionMode::Print => "print",
                            ExtensionMode::Json => "json",
                            _ => "interactive",
                        }),
                        ctx.model.is_some(),
                        false,
                    );
                    *lifecycle_delivery
                        .lock()
                        .map_err(|_| ExtensionFailure::new("terminal delivery state poisoned"))? =
                        delivery_mode;
                    notifier
                        .lock()
                        .map_err(|_| ExtensionFailure::new("monitor notifier state poisoned"))?
                        .take();
                    if matches!(ctx.mode, ExtensionMode::Print | ExtensionMode::Json) {
                        return Ok(EventResult::None);
                    }
                    let available = lifecycle_delivery.clone();
                    monitors
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .set_runaway_guard(settings.monitor_runaway);
                    let rate_monitors = monitors.clone();
                    let default_rate = settings.monitor.wakes_per_hour;
                    let wakes_per_hour: crate::monitor_notify::WakesPerHour = Box::new(move |id| {
                        let pacing = rate_monitors
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .wake_pacing(id)?;
                        Some(pacing.wakes_per_hour.unwrap_or(default_rate))
                    });
                    let spill_monitors = monitors.clone();
                    let spill_ctx = ctx.clone();
                    let warned = Mutex::new(std::collections::BTreeSet::<String>::new());
                    let spill: crate::monitor_overflow::MonitorSpill =
                        Box::new(move |id, lines| {
                            spill_monitor_overflow(&spill_ctx, &spill_monitors, &warned, id, lines)
                        });
                    let delivery = crate::monitor_notify::MonitorNotifier::with_pacing(
                        settings.monitor,
                        move || available.lock().expect("terminal delivery").is_some(),
                        Some(wakes_per_hour),
                        Some(spill),
                        move |injection| {
                            let Some(delivery_mode) =
                                *lifecycle_delivery.lock().expect("terminal delivery")
                            else {
                                return;
                            };
                            if !injection.pause_ids.is_empty() {
                                monitors
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .pause(&injection.pause_ids);
                            }
                            let force_wake = !injection.pause_ids.is_empty();
                            if let Err(error) = sender.send_message(
                                CustomMessage {
                                    custom_type:
                                        crate::monitor_notify::MONITOR_NOTIFICATION_CUSTOM_TYPE
                                            .to_owned(),
                                    content: vec![ToolContent::text(injection.content)],
                                    display: false,
                                    details: Some(injection.details),
                                },
                                SendMessageOptions {
                                    trigger_turn: true,
                                    deliver_as: Some(
                                        if delivery_mode
                                            == crate::notify::NotificationDelivery::Steer
                                            || force_wake
                                        {
                                            DeliverAs::Steer
                                        } else {
                                            DeliverAs::FollowUp
                                        },
                                    ),
                                },
                            ) {
                                maho_ext_api::diagnostic(&format!(
                                    "monitor notification failed: {error}"
                                ));
                            }
                        },
                    );
                    *notifier
                        .lock()
                        .map_err(|_| ExtensionFailure::new("monitor notifier state poisoned"))? =
                        Some(delivery);
                    Ok(EventResult::None)
                })
            }),
        );
        let delivery_monitors = monitors.clone();
        let delivery_notifier = notifier.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, _| {
                let monitors = delivery_monitors.clone();
                let notifier = delivery_notifier.clone();
                Box::pin(async move {
                    monitors
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .bind_delivery(move |event| {
                            if let Some(notifier) =
                                notifier.lock().expect("monitor notifier").as_ref()
                                && let Err(error) = notifier.notify_event(event)
                            {
                                maho_ext_api::diagnostic(&format!(
                                    "monitor delivery failed: {error}"
                                ));
                            }
                        });
                    Ok(EventResult::None)
                })
            }),
        );
        let bash_manager = Arc::clone(&manager);
        let bash_configuration = configuration.clone();
        let completion_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(vec![]));
        let bash_tasks = completion_tasks.clone();
        let cleanup_terminal_notifier = terminal_notifier.clone();
        let restored_manager = manager.clone();
        let restored_backgrounds = backgrounds.clone();
        let restored_sender = bash_sender.clone();
        let restored_delivery = completion_delivery.clone();
        let restored_notifier = terminal_notifier.clone();
        let restored_tasks = completion_tasks.clone();
        let restored_lifecycle = state_lifecycle.clone();
        let restored_recorded = recorded_background_ids.clone();
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, _| {
                let manager = restored_manager.clone();
                let backgrounds = restored_backgrounds.clone();
                let sender = restored_sender.clone();
                let delivery = restored_delivery.clone();
                let notifier = restored_notifier.clone();
                let tasks = restored_tasks.clone();
                let lifecycle = restored_lifecycle.clone();
                let recorded = restored_recorded.clone();
                Box::pin(async move {
                    publish_backgrounds(&backgrounds, &sender);
                    let entries = backgrounds
                        .lock()
                        .map_err(|_| ExtensionFailure::new("terminal backgrounds poisoned"))?
                        .values()
                        .cloned()
                        .collect::<Vec<_>>();
                    // Upstream `onBackgroundState`: each live background session is recorded once per writer.
                    crate::extension_state::record_background_starts(
                        &lifecycle,
                        &recorded,
                        entries.clone(),
                    )
                    .await;
                    let ids = entries
                        .into_iter()
                        .map(|entry| entry.id)
                        .collect::<Vec<_>>();
                    for id in ids {
                        if let Some(task) = bind_completion(
                            id,
                            manager.clone(),
                            backgrounds.clone(),
                            sender.clone(),
                            delivery.clone(),
                            notifier.clone(),
                            lifecycle.clone(),
                        ) {
                            tasks
                                .lock()
                                .map_err(|_| {
                                    ExtensionFailure::new("terminal completion tasks poisoned")
                                })?
                                .push(task);
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let bash_backgrounds = backgrounds.clone();
        let bash_lifecycle = state_lifecycle.clone();
        let bash_ensure = ensure_persistence.clone();
        // The tool context the child placement reads (upstream `buildToolContext`): the live configuration, the
        // session cwd, the shell environment, and the hooks a background session that lives on the child reports to.
        let terminal_context = crate::extension_state::build_tool_context(&terminal_state);
        let mut bash = ToolDefinition::new(
            "bash",
            r#"Execute a shell command in a persistent PTY-backed session. Set run_in_background:true for long-lived or interactive sessions; steer them with bash_input, snapshot with bash_output, tear down with kill_bash. To wait on observable state (a build finishing, a server coming up, a log line), never run sleep or poll loops: subscribe with the monitor tool instead. Foreground blocking stops at the ~60s window and a still-running command auto-detaches to a live background session; `timeout` is the process kill deadline in seconds."#,
            crate::tools::bash::pty_bash_schema(),
            Arc::new(move |call| {
                let manager = Arc::clone(&bash_manager);
                let sender = bash_sender.clone();
                let delivery = completion_delivery.clone();
                let notifier = terminal_notifier.clone();
                let tasks = bash_tasks.clone();
                let bash_configuration = bash_configuration.clone();
                let backgrounds = bash_backgrounds.clone();
                let lifecycle = bash_lifecycle.clone();
                let ensure = bash_ensure.clone();
                Box::pin(async move {
                    let (settings, shell) = bash_configuration
                        .lock()
                        .map_err(|_| {
                            ToolError::Message("terminal configuration state poisoned".to_owned())
                        })?
                        .clone();
                    let description = call
                        .params
                        .get("description")
                        .or_else(|| call.params.get("command"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let started_at_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_err(|error| ToolError::Message(error.to_string()))?
                        .as_secs_f64()
                        * 1000.0;
                    let result = crate::tools::bash::execute_configured_bash(
                        manager.clone(),
                        call,
                        std::env::var("PI_BASH_FOREGROUND_SECONDS").ok().as_deref(),
                        &settings,
                        shell.as_deref(),
                    )
                    .await
                    .map_err(ToolError::Message)?;
                    if let Some(details) = &result.details
                        && details.get("background").and_then(Value::as_bool) == Some(true)
                        && let Some(id) = details.get("bash_id").and_then(Value::as_str)
                    {
                        let id = id.to_owned();
                        backgrounds
                            .lock()
                            .map_err(|_| {
                                ToolError::Message("terminal backgrounds poisoned".to_owned())
                            })?
                            .insert(
                                id.clone(),
                                crate::session_bundle::BackgroundSession {
                                    id: id.clone(),
                                    description: description.clone(),
                                    started_at_ms,
                                },
                            );
                        publish_backgrounds(&backgrounds, &sender);
                        // Upstream `onBackgroundState`: a live background session with no
                        // recorder bound yet binds persistence first.
                        crate::extension_state::ensure_persistence(&ensure).await;
                        if let Some(lifecycle) = lifecycle.lock().await.as_mut() {
                            lifecycle
                                .writer
                                .record_background_start(
                                    &id,
                                    &description,
                                    started_at_ms,
                                    manifest_now_ms(),
                                )
                                .await;
                        }
                        if let Some(task) = bind_completion(
                            id,
                            manager,
                            backgrounds,
                            sender,
                            delivery,
                            notifier,
                            lifecycle,
                        ) {
                            let mut tasks = tasks.lock().map_err(|_| {
                                ToolError::Message("terminal completion tasks poisoned".to_owned())
                            })?;
                            tasks.retain(|task| !task.is_finished());
                            tasks.push(task);
                        }
                    }
                    tool_result(result)
                })
            }),
        );
        bash.exposure = Some(maho_tools::definition::ToolExposure::Eval);
        bash.prompt_snippet = Some(
            "Run shell commands; run_in_background:true for long-lived/interactive PTY sessions"
                .to_owned(),
        );
        bash.prompt_guidelines = Some(vec![
            "You can inspect PI_* environment variables for current model and session details."
                .to_owned(),
        ]);
        bash.placement = Some(maho_tools::definition::ToolPlacement::Child);
        bash.child_job = Some(crate::tools::child_placement::terminal_child_job(
            &terminal_context,
            crate::tools::child_placement::TERMINAL_CHILD_JOB_KINDS.bash,
        ));
        api.register_tool(bash);
        let child_context = terminal_context;
        for (name, description, prompt_snippet, parameters) in [
            (
                "bash_output",
                "Read output from a background bash session without blocking: new output since the last read, the status line, or a rendered full-screen snapshot via view:'screen'. Pattern watches run through monitor; completion arrives as a notification carrying the exit code and output tail.",
                "Peek at background bash session output (filter, screen view, status); watch patterns with monitor, completion arrives as a notification",
                crate::tools::bash_output::bash_output_schema(),
            ),
            (
                "bash_input",
                "Write stdin or named keys (ctrl+c, enter, up, ...) to a live background bash session: e.g. drive a REPL or send SIGINT. This executes arbitrary shell input and is permission-gated like bash.",
                "Send stdin/keys to a live background bash session (REPL steering, ctrl+c, etc.)",
                crate::tools::bash_input::bash_input_schema(),
            ),
            (
                "bash_resize",
                "Resize a live background bash session's PTY (cols x rows) so full-screen TUIs reflow. No-ops with a note under the pipe fallback.",
                "Resize a background bash session's PTY so TUIs reflow",
                crate::tools::bash_resize::bash_resize_schema(),
            ),
            (
                "kill_bash",
                "Terminate a background bash session (or all of them) and its process tree cleanly.",
                "Tree-kill a background bash session (or all) with no orphans",
                crate::tools::kill_bash::kill_bash_schema(),
            ),
        ] {
            let manager = Arc::clone(&manager);
            let monitors = monitors.clone();
            let kill_session_ctx = session_ctx.clone();
            let mut tool = ToolDefinition::new(
                name,
                description,
                parameters,
                Arc::new(move |call| {
                    let manager = Arc::clone(&manager);
                    let monitors = monitors.clone();
                    let kill_ctx = kill_session_ctx.clone();
                    Box::pin(async move {
                        let id = call.params.get("bash_id").and_then(Value::as_str);
                        if name == "kill_bash" {
                            // kill-bash.ts awaits the stop/teardown: the exit graces are awaited with the
                            // manager lock released so the async runtime thread is never blocked.
                            let all = call
                                .params
                                .get("all")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            let terminal_dir = crate::monitor_state_dir::terminal_state_dir(
                                kill_ctx
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .as_ref(),
                            );
                            return tool_result(
                                crate::tools::kill_bash::execute_kill_bash_with_monitors(
                                    &manager,
                                    &monitors,
                                    terminal_dir.as_deref(),
                                    id,
                                    all,
                                )
                                .await,
                            );
                        }
                        let mut manager = manager
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let id = id.ok_or_else(|| {
                            ToolError::Message("bash_id must be a string".to_owned())
                        })?;
                        let resolved = crate::tools::context::resolve_terminal_id(&manager, id);
                        let runtime = manager.get(&resolved);
                        match name {
                            "bash_output" => {
                                let mut result = execute_bash_output_view(
                                    runtime.as_deref(),
                                    id,
                                    call.params.get("filter").and_then(Value::as_str),
                                    call.params.get("view").and_then(Value::as_str)
                                        == Some("screen"),
                                )
                                .map_err(|error| ToolError::Message(error.to_string()))?;
                                let registry = monitors
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                if runtime.is_some()
                                    && let Some(entry) = registry
                                        .snapshot()
                                        .iter()
                                        .find(|entry| entry.id == resolved)
                                {
                                    result = crate::tools::bash_output::with_monitor_state(
                                        result,
                                        id,
                                        entry.paused,
                                        registry.muted_dropped(&resolved),
                                    );
                                }
                                tool_result(result)
                            }
                            "bash_input" => {
                                let keys = call
                                    .params
                                    .get("keys")
                                    .and_then(Value::as_array)
                                    .map(|keys| {
                                        keys.iter()
                                            .filter_map(Value::as_str)
                                            .map(str::to_owned)
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();
                                tool_result(execute_bash_input(
                                    runtime,
                                    BashInputInput {
                                        bash_id: id,
                                        input: call.params.get("input").and_then(Value::as_str),
                                        keys: &keys,
                                        submit: call.params.get("submit").and_then(Value::as_bool),
                                    },
                                ))
                            }
                            "bash_resize" => tool_result(execute_bash_resize(
                                runtime.as_deref(),
                                id,
                                call.params
                                    .get("cols")
                                    .and_then(Value::as_f64)
                                    .unwrap_or(f64::NAN),
                                call.params
                                    .get("rows")
                                    .and_then(Value::as_f64)
                                    .unwrap_or(f64::NAN),
                            )),
                            _ => unreachable!(),
                        }
                    })
                }),
            );
            tool.prompt_snippet = Some(prompt_snippet.to_owned());
            let kinds = &crate::tools::child_placement::TERMINAL_CHILD_JOB_KINDS;
            tool.placement = Some(maho_tools::definition::ToolPlacement::Child);
            tool.child_job = Some(crate::tools::child_placement::terminal_child_job(
                &child_context,
                match name {
                    "bash_output" => kinds.output,
                    "bash_input" => kinds.input,
                    "bash_resize" => kinds.resize,
                    _ => kinds.kill,
                },
            ));
            if name == "bash_output" {
                api.register_tool_with_renderers(tool, crate::tools::render::output_renderers())
                    .expect("valid builtin bash_output registration");
            } else {
                api.register_tool(tool);
            }
        }
        let tool_manager = manager.clone();
        let tool_monitors = monitors.clone();
        let tool_notifier = notifier.clone();
        let tool_configuration = configuration.clone();
        let tool_lifecycle = state_lifecycle.clone();
        let tool_ensure = ensure_persistence.clone();
        let mut monitor_tool = ToolDefinition::new(
            "monitor",
            "Subscribe to a change instead of polling. Pass one source: command watches a PTY session, injecting matching newline-terminated output lines (stderr merged) plus an exit summary; path natively watches one file and fires once, where create (the default) fires only when the file appears after registration, so use event modify for an existing file, and filter is rejected; target (type rpc) watches sessions' status, effects and content on every RPC host and terminal (\"children\", \"all\" or one session). Returns a bash_id immediately; peek with bash_output, stop with kill_bash.",
            crate::tools::monitor::monitor_schema(),
            Arc::new(move |call| {
                let manager = tool_manager.clone();
                let monitors = tool_monitors.clone();
                let notifier = tool_notifier.clone();
                let configuration = tool_configuration.clone();
                let lifecycle = tool_lifecycle.clone();
                let ensure = tool_ensure.clone();
                Box::pin(async move {
                    let cwd = call
                        .context
                        .map(|context| context.cwd().to_path_buf())
                        .unwrap_or(std::env::current_dir()?);
                    let approved_parent = crate::tools::monitor::approved_parent_for(&call)?;
                    let (settings, shell) = configuration
                        .lock()
                        .map_err(|_| {
                            ToolError::Message("terminal configuration state poisoned".to_owned())
                        })?
                        .clone();
                    // Upstream monitor.ts:164/:315 hand the captured spec to the session's manifest writer
                    // and :207-217 caps durable creates before any spawn. The writer lock is async; the
                    // manager/registry locks are held only across the synchronous registration.
                    // Upstream monitor.ts:189-215: a create names exactly one source and passes
                    // the pure checks (wakes_per_hour, rpc human_input) BEFORE persistence is
                    // bound, so a refused create takes no lease and writes no state. Only a
                    // valid rpc, file or command create binds persistence (lazy lease +
                    // recorder), before durable admission and before the writer lock is taken
                    // (the bind installs the lifecycle under that lock).
                    let source =
                        if call.params.get("action").and_then(Value::as_str) != Some("rearm") {
                            let source =
                                match crate::tools::monitor_rpc::monitor_source_of(&call.params) {
                                    Ok(source) => source,
                                    Err(error) => return tool_result(error_result(error)),
                                };
                            if let Some(refused) =
                                crate::tools::monitor_rpc::precheck_create(&call.params, source)
                            {
                                return tool_result(refused);
                            }
                            if source == crate::tools::monitor_rpc::MonitorSource::Rpc
                                || crate::tools::monitor_schema::is_file_create_input(&call.params)
                                || crate::tools::monitor_schema::is_create_input(&call.params)
                            {
                                crate::extension_state::ensure_persistence(&ensure).await;
                            }
                            Some(source)
                        } else {
                            None
                        };
                    let mut lifecycle = lifecycle.lock().await;
                    let session_key =
                        crate::tools::monitor_manifest_binding::manifest_session_key(call.context);
                    // The rpc branch (monitor-rpc.ts) arms a watch over every RPC host and terminal.
                    if let Some(crate::tools::monitor_rpc::MonitorSource::Rpc) = source {
                        return tool_result(
                            create_rpc_monitor_call(
                                &call.params,
                                &settings,
                                session_key,
                                &manager,
                                &monitors,
                                lifecycle.as_mut(),
                            )
                            .await,
                        );
                    }
                    if let Some(refused) = crate::tools::monitor::durable_admission_error(
                        &call.params,
                        settings.max_durable_monitors,
                        session_key.as_deref(),
                        lifecycle.as_ref().map(|lifecycle| &lifecycle.writer),
                    ) {
                        return tool_result(refused);
                    }
                    // Upstream `registerFile` is awaited: the file branch proves its target
                    // with no manager/registry lock held across the await.
                    let file_result =
                        if crate::tools::monitor_schema::is_file_create_input(&call.params)
                            && call.params.get("action").and_then(Value::as_str) != Some("rearm")
                        {
                            Some(
                                crate::tools::monitor::create_file_monitor(
                                    &manager,
                                    &monitors,
                                    &call.params,
                                    &cwd,
                                    approved_parent.as_deref(),
                                )
                                .await,
                            )
                        } else {
                            None
                        };
                    let (result, recorded) = {
                        let mut manager = manager
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let mut monitors = monitors
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let result = match file_result {
                            Some(result) => result,
                            None => crate::tools::monitor::execute_configured_monitor_in(
                                &mut manager,
                                &mut monitors,
                                &call.params,
                                &cwd,
                                approved_parent.as_deref(),
                                shell.as_deref(),
                                &settings,
                                lifecycle
                                    .as_ref()
                                    .and_then(|lifecycle| lifecycle.writer.terminal_dir()),
                            ),
                        };
                        if call.params.get("action").and_then(Value::as_str) == Some("rearm")
                            && let Some(notifier) = notifier
                                .lock()
                                .map_err(|_| {
                                    ToolError::Message("monitor notifier state poisoned".to_owned())
                                })?
                                .as_ref()
                        {
                            notifier
                                .resume(
                                    monitors
                                        .snapshot()
                                        .iter()
                                        .filter(|record| !record.paused)
                                        .map(|record| record.id.clone())
                                        .collect(),
                                )
                                .map_err(ToolError::Message)?;
                        }
                        let registration = crate::tools::monitor::monitor_registration(
                            &call.params,
                            &result,
                            &cwd,
                            approved_parent.as_deref(),
                        );
                        let recorded = registration.map(|(registration, id)| {
                            let checkpoint =
                                (call.params.get("persistent").and_then(Value::as_bool)
                                    == Some(true))
                                .then(|| monitors.file_checkpoint(&id))
                                .flatten();
                            let deadline_ms =
                                crate::tools::monitor::registration_deadline_ms(&monitors, &id);
                            let runtime = manager.get(&id).and_then(|runtime| runtime.identity());
                            (
                                registration
                                    .with_deadline_ms(deadline_ms)
                                    .with_runtime(runtime),
                                checkpoint,
                            )
                        });
                        (result, recorded)
                    };
                    if let Some(lifecycle) = lifecycle.as_mut()
                        && let Some((registration, checkpoint)) = recorded
                    {
                        let monitor_id = registration.registration.monitor_id.clone();
                        lifecycle
                            .writer
                            .record_register_with(registration, manifest_now_ms())
                            .await;
                        if let Some(checkpoint) = checkpoint {
                            lifecycle
                                .writer
                                .schedule_checkpoint(&monitor_id, checkpoint);
                        }
                    } else if let Some((registration, _)) = recorded {
                        // Upstream `handMonitorSpec`: no writer is bound yet (the lease is held
                        // elsewhere), so the spec waits, bounded, for the writer that binds next.
                        if crate::tools::monitor_manifest_binding::hand_monitor_spec(
                            session_key.as_deref(),
                            registration.registration,
                        )
                        .is_some()
                        {
                            maho_ext_api::diagnostic(
                                "terminal manifest: a writer is bound for this session but not held by this generation; the monitor spec was not recorded",
                            );
                        }
                    }
                    drop(lifecycle);
                    tool_result(result)
                })
            }),
        );
        monitor_tool.prompt_snippet=Some("Subscribe to a command's output or a file's create/modify event as injected events instead of polling".to_owned());
        monitor_tool.prompt_guidelines=Some(vec![
            "Waiting on observable state (CI checks, builds, log patterns, deploys, a file landing) means a monitor, never a foreground sleep/poll loop.".to_owned(),
            r#"Waiting for one file to appear or change is the path branch: `monitor({ description, path, event? })` beats wrapping `test -f` in a shell poll loop; a file that already exists needs `event: "modify"`, since `create` only fires on appearance, and registration needs the parent directory to exist already; when the run creates that directory too, use the `command` branch instead."#.to_owned(),
            r#"Shape the command for the events you need: one-shot gate = `until <cond>; do sleep 1; done; printf 'READY\n'` with filter ^READY$; stream = `tail -n 0 -F <log> | grep --line-buffered <pat>` with persistent: true, then kill_bash."#.to_owned(),
            "A non-persistent command watch is a watcher with a deadline: timeout_ms KILLS the watched command when it fires (the summary says killed at deadline). Put long jobs in a run_in_background session and only watch them.".to_owned(),
            r#"Waiting on other sessions (your children, a task, every session) is the rpc branch: `monitor({ type: "rpc", target: "children", description: "children status" })` reports each status change and effect receipt as it happens; `events: "all"` adds their content (streaming collapses to one line per 2 s), `exclude` drops kinds."#.to_owned(),
            "Sleep loops belong INSIDE the monitor command, never in your turn: about to sleep, re-poll bash_output, or foreground-block on a long command means register a monitor and keep working.".to_owned(),
        ]);
        api.register_tool_with_renderers(monitor_tool, crate::tools::render::monitor_renderers())
            .expect("valid builtin monitor registration");
        crate::restore_digest::register_restore_digest_renderer(api);
        let activity = notifier.clone();
        let input_monitors = monitors.clone();
        let input_digests = digests.clone();
        let input_sender = sender.clone();
        api.on(EventKind::Input,Arc::new(move |event,ctx| {let notifier=activity.clone();let monitors=input_monitors.clone();let digests=input_digests.clone();let sender=input_sender.clone();Box::pin(async move {
            if matches!(event,maho_ext_api::types::ExtensionEvent::Input(event) if event.source==maho_ext_api::types::InputSource::Extension) {return Ok(EventResult::None);}
            flush_restore_digest(&digests,&sender,ctx,ctx.model.is_some());
            let resumed=monitors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resume(None);
            if let Some(notifier)=notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.as_ref() {notifier.note_activity().map_err(ExtensionFailure::new)?;if !resumed.is_empty() {notifier.resume(resumed.into_iter().map(|(id,_)|id).collect()).map_err(ExtensionFailure::new)?;}}
            Ok(EventResult::None)
        })}));
        let activity = notifier.clone();
        api.on(
            EventKind::ToolCall,
            Arc::new(move |_, _| {
                let notifier = activity.clone();
                Box::pin(async move {
                    if let Some(notifier) = notifier
                        .lock()
                        .map_err(|_| ExtensionFailure::new("monitor notifier state poisoned"))?
                        .as_ref()
                    {
                        notifier.note_activity().map_err(ExtensionFailure::new)?;
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        let shutdown_bundle = bundle.clone();
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                let tasks = completion_tasks.clone();
                let notifier = cleanup_terminal_notifier.clone();
                Box::pin(async move {
                    for task in tasks
                        .lock()
                        .map_err(|_| ExtensionFailure::new("terminal completion tasks poisoned"))?
                        .drain(..)
                    {
                        task.abort();
                    }
                    *notifier
                        .lock()
                        .map_err(|_| ExtensionFailure::new("terminal notifier state poisoned"))? =
                        crate::notify::TerminalNotifier::default();
                    Ok(EventResult::None)
                })
            }),
        );
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, _| {
                let task = telemetry_task.clone();
                Box::pin(async move {
                    if let Some(task) = task
                        .lock()
                        .map_err(|_| ExtensionFailure::new("monitor telemetry state poisoned"))?
                        .take()
                    {
                        task.abort();
                    }
                    Ok(EventResult::None)
                })
            }),
        );
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, ctx| {
                let status = status_task.clone();
                Box::pin(async move {
                    let task = status
                        .lock()
                        .map_err(|_| ExtensionFailure::new("monitor status state poisoned"))?
                        .take();
                    if let Some(task) = task {
                        task.abort();
                        if let Err(error) = task.await
                            && !error.is_cancelled()
                        {
                            return Err(ExtensionFailure::new(error.to_string()));
                        }
                    }
                    ctx.ui
                        .set_status(crate::monitor_status::MONITOR_STATUS_KEY, None);
                    Ok(EventResult::None)
                })
            }),
        );
        api.on(EventKind::SessionShutdown,Arc::new(move |event,ctx| {let bundle=shutdown_bundle.clone();let notifier=notifier.clone();let sender=shutdown_sender.clone();Box::pin(async move {
            if matches!(event,maho_ext_api::types::ExtensionEvent::SessionShutdown(event) if event.reason!=maho_ext_api::types::SessionReason::Reload) {notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.take();}
            let session_id=ctx.session_manager.session_id().to_owned();
            if matches!(event,maho_ext_api::types::ExtensionEvent::SessionShutdown(event) if event.reason==maho_ext_api::types::SessionReason::Reload) {
                bundle.lock().map_err(|_|ExtensionFailure::new("terminal bundle state poisoned"))?.monitors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).detach_delivery();
                crate::session_bundle::park_bundle(&session_id,bundle.lock().map_err(|_|ExtensionFailure::new("terminal bundle state poisoned"))?.clone()).map_err(|error|ExtensionFailure::new(error.to_string()))?;
            }else {
                // registry.ts teardown is awaited: release the bundle lock so none is held
                // across the exit-grace awaits and the runtime thread is not blocked.
                let manager={let mut live=bundle.lock().map_err(|_|ExtensionFailure::new("terminal bundle state poisoned"))?;live.begin_teardown().map_err(|error|ExtensionFailure::new(error.to_string()))?.then(||live.manager.clone())};
                // Upstream teardown disposes the registry and `onMonitorState([])` publishes the empty
                // set. Here the telemetry task that would forward it is already aborted, so publish it
                // directly; otherwise wake-source consumers keep the last non-zero monitor count.
                if manager.is_some() {
                    let payload=monitor_state_payload(&[]);
                    sender.events.emit("terminal_monitor_state",&payload);
                    if let Err(error)=sender.rpc_emit("terminal_monitor_state",&payload) {maho_ext_api::diagnostic(&format!("monitor state RPC delivery failed: {error}"));}
                    sender.events.emit("wake_source_state",&serde_json::json!({"source":"terminal-monitors","activeCount":0,"monitors":[]}));
                }
                if let Some(manager)=manager {crate::manager::teardown_shared(&manager).await.map_err(|error|ExtensionFailure::new(error.to_string()))?;}
                crate::session_bundle::teardown_parked_bundle_async(&session_id).await.map_err(|error|ExtensionFailure::new(error.to_string()))?;
            }
            Ok(EventResult::None)
        })}));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::types::*;
    #[tokio::test]
    async fn registered_file_monitor_accessor_error_creates_no_registration()
    -> Result<(), ToolError> {
        // Given a real registered ingress and an accessor that fails admission.
        let dir = tempfile::tempdir()?;
        let mut api = ExtensionApi::new(
            LoadedExtension::new("terminal", dir.path().to_owned(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        TerminalExtension.register(&mut api);
        let context = crate::tools::monitor::tests::stub(Err("admission unavailable".into()));
        let monitor = &api.registered.tools[5].definition;
        // When the registered tool is invoked, the carrier error must escape.
        let error = (monitor.execute)(maho_tools::definition::ToolCall {
            id: "rejected",
            params: json!({"description":"watch","path":dir.path().join("file"),"persistent":true}),
            signal: Default::default(),
            on_update: None,
            context: Some(&context),
        })
        .await
        .expect_err("accessor error must escape registered ingress");
        assert!(error.to_string().contains("admission unavailable"));
        // Then kill-all observes zero registrations, and the first admitted file keeps watch_1.
        let empty =
            (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {
                id: "empty",
                params: json!({"all":true}),
                signal: Default::default(),
                on_update: None,
                context: None,
            })
            .await?;
        assert!(
            matches!(&empty.content[0],ToolContent::Text {text,..} if text=="Killed 0 session(s).")
        );
        let context = crate::tools::monitor::tests::stub(Ok(None));
        let admitted = (monitor.execute)(maho_tools::definition::ToolCall {
            id: "admitted",
            params: json!({"description":"watch","path":dir.path().join("file"),"persistent":true}),
            signal: Default::default(),
            on_update: None,
            context: Some(&context),
        })
        .await?;
        assert_eq!(admitted.details.as_ref().unwrap()["bash_id"], "watch_1");
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {
            id: "cleanup",
            params: json!({"all":true}),
            signal: Default::default(),
            on_update: None,
            context: None,
        })
        .await?;
        Ok(())
    }
    /// monitor.ts: a create names one source (`monitorSourceOf`), and the rpc branch refuses
    /// `human_input` before arming anything.
    #[tokio::test]
    async fn registered_monitor_resolves_one_source_and_refuses_rpc_human_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut api = ExtensionApi::new(
            LoadedExtension::new("terminal", dir.path().to_owned(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        TerminalExtension.register(&mut api);
        let monitor = &api.registered.tools[5].definition;
        for (params, expected) in [
            (
                json!({"description":"d","command":"true","path":"file"}),
                "monitor takes one source; this call names command and path.",
            ),
            (
                json!({"description":"d","type":"rpc"}),
                "monitor type \"rpc\" needs target; pass target: \"children\", \"all\" or a session id.",
            ),
            (
                json!({"description":"d","target":"all","human_input":true}),
                "human_input applies to command monitors; an rpc watch reports session status, not a person's text.",
            ),
            // monitor.ts:189: wakes_per_hour is refused on an rpc watch, ahead of human_input.
            (
                json!({"description":"d","target":"all","wakes_per_hour":12,"human_input":true}),
                "wakes_per_hour applies to persistent command monitors (command with persistent: true); other watches are not wake-paced.",
            ),
            (
                json!({"description":"d","target":"all","persistent":true,"wakes_per_hour":12}),
                "wakes_per_hour applies to persistent command monitors (command with persistent: true); other watches are not wake-paced.",
            ),
        ] {
            let error = (monitor.execute)(maho_tools::definition::ToolCall {
                id: "source",
                params,
                signal: Default::default(),
                on_update: None,
                context: None,
            })
            .await
            .expect_err("refused before arming");
            assert_eq!(error.to_string(), expected);
        }
    }
    #[tokio::test]
    async fn registered_file_monitor_stable_id_kill_releases_shared_capacity()
    -> Result<(), ToolError> {
        let dir = tempfile::tempdir()?;
        let mut api = ExtensionApi::new(
            LoadedExtension::new("terminal", dir.path().to_owned(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        TerminalExtension.register(&mut api);
        let context = crate::tools::monitor::tests::stub(Ok(None));
        let result=(api.registered.tools[5].definition.execute)(maho_tools::definition::ToolCall {id:"file",params:json!({"description":"watch","path":dir.path().join("file"),"persistent":true}),signal:Default::default(),on_update:None,context:Some(&context)}).await?;
        let id = result.details.as_ref().unwrap()["monitor_id"]
            .as_str()
            .unwrap();
        assert!(id.starts_with("mon_"));
        let result =
            (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {
                id: "kill",
                params: json!({"bash_id":id}),
                signal: Default::default(),
                on_update: None,
                context: None,
            })
            .await?;
        assert!(
            matches!(&result.content[0],ToolContent::Text {text,..} if text==&format!("Killed {id}."))
        );
        let result=(api.registered.tools[5].definition.execute)(maho_tools::definition::ToolCall {id:"next",params:json!({"description":"next","path":dir.path().join("next"),"persistent":true}),signal:Default::default(),on_update:None,context:Some(&context)}).await?;
        assert!(
            result.details.as_ref().unwrap()["monitor_id"]
                .as_str()
                .unwrap()
                .starts_with("mon_")
        );
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {
            id: "all",
            params: json!({"all":true}),
            signal: Default::default(),
            on_update: None,
            context: None,
        })
        .await?;
        Ok(())
    }
    #[tokio::test]
    async fn registry_state_reaches_native_event_bus_and_rpc() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::default();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let state_sender = sender.clone();
        let rpc_sender = sender;
        let _state = bus.on(
            "terminal_monitor_state",
            Arc::new(move |data| {
                state_sender.send(("state", data.clone())).unwrap();
            }),
        );
        let _rpc = bus.on(
            "senpi:extension-rpc-event",
            Arc::new(move |data| {
                rpc_sender.send(("rpc", data.clone())).unwrap();
            }),
        );
        let api = Arc::new(ExtensionApi::new(
            LoadedExtension::new("terminal", dir.path().to_owned(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            bus,
            ExtensionRuntime::default(),
        ));
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let task = bind_monitor_events(
            registry.subscribe_state(),
            api,
            Arc::new(tokio::sync::Mutex::new(None)),
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (_, initial) = events.recv().await.unwrap();
            assert_eq!(initial["activeCount"], 0);
            let (kind, rpc) = events.recv().await.unwrap();
            assert_eq!(kind, "rpc");
            assert_eq!(rpc["data"], initial);
            let (id, _) = registry
                .register_persistent_file(
                    "watch",
                    &dir.path().join("file"),
                    crate::terminal_manifest_model::FileEvent::Create,
                )
                .unwrap();
            let (kind, live) = events.recv().await.unwrap();
            assert_eq!(kind, "state");
            assert_eq!(live["activeCount"], 1);
            assert_eq!(live["monitors"][0]["id"], id);
            assert_eq!(live["monitors"][0]["persistent"], true);
            assert_eq!(live["monitors"][0].get("command"), Some(&Value::Null));
            assert_eq!(live["monitors"][0]["fireCount"], 0);
            let (_, rpc) = events.recv().await.unwrap();
            assert_eq!(rpc["data"], live);
            registry.stop_file(&id);
            let (_, empty) = events.recv().await.unwrap();
            assert_eq!(empty["activeCount"], 0);
            assert_eq!(events.recv().await.unwrap().1["data"], empty);
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }
    #[tokio::test]
    async fn registered_bash_runs_real_pty_and_companion_peeks() -> Result<(), ToolError> {
        let tmp = std::env::temp_dir();
        let mut api = ExtensionApi::new(
            LoadedExtension::new("terminal", tmp, SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        TerminalExtension.register(&mut api);
        let result =
            (api.registered.tools[0].definition.execute)(maho_tools::definition::ToolCall {
                id: "c1",
                params: json!({"command":"stty -echo; printf 'ready\\n'"}),
                signal: Default::default(),
                on_update: None,
                context: None,
            })
            .await?;
        assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text=="ready"));
        let id = "bash_1";
        let result =
            (api.registered.tools[1].definition.execute)(maho_tools::definition::ToolCall {
                id: "c2",
                params: json!({"bash_id":id}),
                signal: Default::default(),
                on_update: None,
                context: None,
            })
            .await?;
        assert!(
            matches!(&result.content[0],ToolContent::Text {text,..} if text.contains("status: completed exit_code: 0"))
        );
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {
            id: "c3",
            params: json!({"all":true}),
            signal: Default::default(),
            on_update: None,
            context: None,
        })
        .await?;
        Ok(())
    }
    #[test]
    fn native_companions_register_flat_schemas_and_shutdown() {
        let tmp = std::env::temp_dir();
        let mut api = ExtensionApi::new(
            LoadedExtension::new("terminal", tmp, SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        TerminalExtension.register(&mut api);
        assert_eq!(
            api.registered
                .tools
                .iter()
                .map(|tool| tool.definition.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "bash",
                "bash_output",
                "bash_input",
                "bash_resize",
                "kill_bash",
                "monitor"
            ]
        );
        for tool in &api.registered.tools {
            assert_eq!(tool.definition.parameters["type"], "object");
            assert!(tool.definition.parameters.get("properties").is_some());
        }
        assert_eq!(
            api.registered.handlers[&EventKind::SessionShutdown].len(),
            5
        );
        assert!(
            api.registered
                .message_renderers
                .contains_key(crate::restore_digest::RESTORE_DIGEST_CUSTOM_TYPE)
        );
    }
    #[tokio::test]
    async fn monitor_ending_reaches_native_event_bus_and_rpc() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::default();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let ended_sender = sender.clone();
        let rpc_sender = sender;
        let _ended = bus.on(
            crate::shared::TERMINAL_MONITOR_ENDED_EVENT,
            Arc::new(move |data| {
                ended_sender.send(("ended", data.clone())).unwrap();
            }),
        );
        let _rpc = bus.on(
            "senpi:extension-rpc-event",
            Arc::new(move |data| {
                rpc_sender.send(("rpc", data.clone())).unwrap();
            }),
        );
        let mut api = ExtensionApi::new(
            LoadedExtension::new("terminal", dir.path().to_owned(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            bus,
            ExtensionRuntime::default(),
        );
        TerminalExtension.register(&mut api);
        // `stty` is absent or does not return under ConPTY, so the shell never reaches printf and never exits.
        let result =
            (api.registered.tools[5].definition.execute)(maho_tools::definition::ToolCall {
                id: "monitor",
                params: json!({"description":"ready","command":"printf 'ready\\n'"}),
                signal: Default::default(),
                on_update: None,
                context: None,
            })
            .await
            .unwrap();
        assert!(
            result.details.as_ref().unwrap()["monitor_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("mon_"))
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut seen_ended = false;
            let mut seen_rpc = false;
            while !(seen_ended && seen_rpc) {
                let (kind, data) = events.recv().await.unwrap();
                if kind == "ended" {
                    assert_eq!(data["reason"], "exit");
                    assert_eq!(data["exitCode"].as_i64(), Some(0));
                    seen_ended = true;
                } else if kind == "rpc"
                    && data["name"] == crate::shared::TERMINAL_MONITOR_ENDED_EVENT
                {
                    assert_eq!(data["data"]["reason"], "exit");
                    seen_rpc = true;
                }
            }
        })
        .await
        .unwrap();
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {
            id: "cleanup",
            params: json!({"all":true}),
            signal: Default::default(),
            on_update: None,
            context: None,
        })
        .await
        .unwrap();
    }
    struct NotificationCaptureActions {
        sent: Arc<Mutex<Vec<CustomMessage>>>,
    }
    impl ExtensionActions for NotificationCaptureActions {
        fn send_message(
            &self,
            message: CustomMessage,
            _options: SendMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            self.sent.lock().unwrap().push(message);
            Ok(())
        }
        fn send_user_message(
            &self,
            _content: UserMessageContent,
            _options: SendUserMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            Ok(())
        }
        fn append_entry(
            &self,
            _custom_type: &str,
            _data: Option<JsonValue>,
        ) -> Result<(), ExtensionFailure> {
            Ok(())
        }
        fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn bind_completion_preserves_notice_under_capacity_eviction_with_lifecycle_gated() {
        let dir = tempfile::tempdir().unwrap();
        let sent_messages = Arc::new(Mutex::new(Vec::new()));
        let runtime = ExtensionRuntime::default();
        runtime.bind(Arc::new(NotificationCaptureActions {
            sent: sent_messages.clone(),
        }));
        let api = Arc::new(ExtensionApi::new(
            LoadedExtension::new("terminal", dir.path().to_owned(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        ));
        let manager = Arc::new(Mutex::new(TerminalManager::new(1)));
        let id1 = manager
            .lock()
            .unwrap()
            .create(
                "cmd1",
                crate::shared::test_sh_options(
                    "stty -echo; printf 'first_completion_tail'; exit 0",
                ),
            )
            .unwrap();
        let backgrounds: Backgrounds = Arc::new(Mutex::new(indexmap::IndexMap::new()));
        backgrounds.lock().unwrap().insert(
            id1.clone(),
            crate::session_bundle::BackgroundSession {
                id: id1.clone(),
                description: "test".to_owned(),
                started_at_ms: 0.0,
            },
        );
        let delivery = Arc::new(Mutex::new(Some(crate::notify::NotificationDelivery::Steer)));
        let notifier = Arc::new(Mutex::new(crate::notify::TerminalNotifier::default()));

        // Hold the lifecycle lock BEFORE bind_completion to gate the completion task at the lifecycle await.
        let lifecycle = Arc::new(tokio::sync::Mutex::new(None));
        let gate = lifecycle.lock().await;

        let mut exit_watch = {
            let mut mgr = manager.lock().unwrap();
            mgr.get(&id1).unwrap().subscribe_exit()
        };

        let task = bind_completion(
            id1.clone(),
            manager.clone(),
            backgrounds.clone(),
            api.clone(),
            delivery.clone(),
            notifier.clone(),
            lifecycle.clone(),
        )
        .expect("task handle");

        // Await session 1 exit before triggering eviction.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while exit_watch.borrow_and_update().is_none() {
                exit_watch.changed().await.expect("exit sender");
            }
        })
        .await
        .expect("id1 exit");

        // While completion callback is gated on the lifecycle lock, capacity eviction occurs:
        // allocating session 2 at max_sessions=1 succeeds without capacity error and evicts id1 from manager.
        let id2 = manager
            .lock()
            .unwrap()
            .create("cmd2", crate::shared::test_sh_options("exit 0"))
            .expect("session creation succeeds without capacity error");
        assert_ne!(id2, id1);
        assert_eq!(manager.lock().unwrap().size(), 1);
        assert!(
            manager.lock().unwrap().get(&id1).is_none(),
            "id1 is evicted from manager"
        );

        // Release the lifecycle gate so completion callback can proceed to view reading and notice sending.
        drop(gate);

        // Await completion task to finish.
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("completion task must finish")
            .expect("join ok");

        // Assert actual sent notification was delivered and contains the session output tail.
        let messages = sent_messages.lock().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "exactly one completion message must be sent"
        );
        assert_eq!(
            messages[0].custom_type,
            crate::notify::TERMINAL_NOTIFICATION_CUSTOM_TYPE
        );
        let content_text = match &messages[0].content[0] {
            ToolContent::Text { text, .. } => text,
            _ => panic!("expected text content"),
        };
        assert!(
            content_text.contains("first_completion_tail"),
            "notification must contain output tail, got: {content_text}"
        );

        manager.lock().unwrap().teardown().unwrap();
    }
}

/// Port of the persistence rows of terminal-restore-session.test.ts, terminal-restore-digest.test.ts
/// and terminal-resumption-channel-state.test.ts (senpi v2026.10.27-2): the restore digest slot and
/// its flush, own/wait/stop around the lease keeper, the post-takeover sweep and the stable
/// `mon_` id on the wake-source channel.
#[cfg(test)]
mod persistence_tests {
    use super::*;
    use maho_ext_api::types::*;
    use std::path::{Path, PathBuf};

    #[derive(Default)]
    struct RecordingUi {
        notes: Mutex<Vec<String>>,
        statuses: Mutex<Vec<(String, Option<String>)>>,
    }
    impl ExtensionUi for RecordingUi {
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
        fn notify(&self, message: &str, _: NotificationType) {
            self.notes.lock().unwrap().push(message.to_owned());
        }
        fn set_status(&self, key: &str, text: Option<&str>) {
            self.statuses
                .lock()
                .unwrap()
                .push((key.to_owned(), text.map(str::to_owned)));
        }
        fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
        fn set_header(&self, _: Option<ComponentFactory>) {}
        fn set_footer(&self, _: Option<ComponentFactory>) {}
        fn set_title(&self, _: &str) {}
        fn paste_to_editor(&self, _: &str) {}
        fn set_editor_text(&self, _: &str) {}
        fn get_editor_text(&self) -> String {
            String::new()
        }
        fn custom(
            &self,
            _: ComponentFactory,
            _: CustomUiOptions,
        ) -> ExtensionFuture<'_, JsonValue> {
            Box::pin(async { Err("unavailable".into()) })
        }
        fn theme(&self) -> Theme {
            Theme::default()
        }
    }

    struct Session {
        dir: PathBuf,
    }
    impl maho_tools::definition::ToolSessionManager for Session {
        fn session_id(&self) -> &str {
            "terminal-persistence"
        }
        fn session_file(&self) -> Option<&Path> {
            None
        }
    }
    impl SessionManager for Session {
        fn get_session_dir(&self) -> Option<PathBuf> {
            Some(self.dir.clone())
        }
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
        fn get_api_key_for_provider<'a>(
            &'a self,
            _: &'a str,
        ) -> ExtensionFuture<'a, Option<String>> {
            Box::pin(async { Ok(None) })
        }
    }

    type Sent = Arc<Mutex<Vec<(CustomMessage, SendMessageOptions)>>>;
    struct Actions {
        sent: Sent,
    }
    impl ExtensionActions for Actions {
        fn send_message(
            &self,
            message: CustomMessage,
            options: SendMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            self.sent.lock().unwrap().push((message, options));
            Ok(())
        }
        fn send_user_message(
            &self,
            _: UserMessageContent,
            _: SendUserMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            Err("unexpected user message".into())
        }
        fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
            Ok(())
        }
        fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
            Ok(vec![])
        }
    }

    fn model() -> Model {
        maho_ai::models_generated::get_builtin_model("google", "gemini-2.5-flash")
            .expect("catalog model")
            .clone()
    }

    fn context(dir: &Path, ui: Arc<RecordingUi>, model: Option<Model>) -> ExtensionContext {
        ExtensionContext {
            ui,
            mode: ExtensionMode::Tui,
            has_ui: true,
            cwd: dir.into(),
            agent_dir: dir.into(),
            session_manager: Arc::new(Session { dir: dir.into() }),
            model_registry: Arc::new(Registry),
            model,
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

    fn sender(dir: &Path) -> (Arc<ExtensionApi>, Sent) {
        let sent: Sent = Arc::default();
        let runtime = ExtensionRuntime::default();
        runtime.bind(Arc::new(Actions { sent: sent.clone() }));
        let api = ExtensionApi::new(
            LoadedExtension::new("terminal", dir.to_owned(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        );
        (Arc::new(api), sent)
    }

    fn digest(
        outcome: crate::restore_digest::RestoreDigestOutcome,
        generation: u64,
    ) -> crate::restore_digest::RestoreDigestMessage {
        crate::restore_digest::build_restore_digest(
            &crate::restore_digest::RestoreDigestInput {
                results: vec![crate::restore_digest::MonitorRestoreResult {
                    monitor_id: "mon_RESTORED00000001".into(),
                    description: "watch".into(),
                    kind: "command".into(),
                    outcome: "lost".into(),
                    command: Some("cat".into()),
                    path: None,
                    reason: None,
                    orphan: None,
                }],
                ..Default::default()
            },
            generation,
            outcome,
            None,
        )
    }

    fn digests_of(sent: &Sent) -> Vec<(JsonValue, SendMessageOptions)> {
        sent.lock()
            .unwrap()
            .iter()
            .filter(|(message, _)| {
                message.custom_type == crate::restore_digest::RESTORE_DIGEST_CUSTOM_TYPE
            })
            .map(|(message, options)| {
                (message.details.clone().unwrap_or_default(), options.clone())
            })
            .collect()
    }

    /// terminal-restore-session.test.ts (f): a decided digest waits for a model, then is delivered
    /// exactly once; an actionable digest wakes the agent as a follow-up.
    #[test]
    fn flush_restore_digest_waits_for_a_model_and_delivers_once() {
        let dir = tempfile::tempdir().unwrap();
        let (api, sent) = sender(dir.path());
        let ui = Arc::new(RecordingUi::default());
        let digests: Digests = Arc::default();
        digests.lock().unwrap().slot.set(digest(
            crate::restore_digest::RestoreDigestOutcome::Decided,
            1,
        ));

        flush_restore_digest(
            &digests,
            &api,
            &context(dir.path(), ui.clone(), None),
            false,
        );
        assert!(
            digests_of(&sent).is_empty(),
            "no model bound: the digest stays pending"
        );
        assert!(digests.lock().unwrap().slot.pending().is_some());

        let ctx = context(dir.path(), ui.clone(), Some(model()));
        flush_restore_digest(&digests, &api, &ctx, true);
        flush_restore_digest(&digests, &api, &ctx, true);
        let delivered = digests_of(&sent);
        assert_eq!(delivered.len(), 1, "delivered exactly once");
        assert_eq!(delivered[0].0["outcome"], "decided");
        assert!(delivered[0].1.trigger_turn);
        assert_eq!(delivered[0].1.deliver_as, Some(DeliverAs::FollowUp));
        assert_eq!(
            ui.notes.lock().unwrap().len(),
            1,
            "the user sees it once too"
        );

        // A deferred note never displaces a decided digest, and none follows a delivered one.
        digests.lock().unwrap().slot.set(digest(
            crate::restore_digest::RestoreDigestOutcome::Deferred,
            1,
        ));
        flush_restore_digest(&digests, &api, &ctx, true);
        assert_eq!(digests_of(&sent).len(), 1);
    }

    /// terminal-restore-digest.test.ts (e): notify off keeps the model channel silent; only the user hears it.
    #[test]
    fn flush_restore_digest_with_notify_off_tells_only_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let (api, sent) = sender(dir.path());
        let ui = Arc::new(RecordingUi::default());
        let digests: Digests = Arc::default();
        {
            let mut state = digests.lock().unwrap();
            state.notify_off = true;
            state.slot.set(digest(
                crate::restore_digest::RestoreDigestOutcome::Decided,
                1,
            ));
        }
        flush_restore_digest(
            &digests,
            &api,
            &context(dir.path(), ui.clone(), None),
            false,
        );
        assert!(sent.lock().unwrap().is_empty());
        assert_eq!(ui.notes.lock().unwrap().len(), 1);
        assert!(digests.lock().unwrap().slot.pending().is_none());
    }

    struct Fixture {
        /// Owns the temp tree for the fixture's lifetime; unix-only tests also read its path.
        #[cfg(unix)]
        dir: tempfile::TempDir,
        #[cfg(not(unix))]
        _dir: tempfile::TempDir,
        session_dir: PathBuf,
        terminal_dir: PathBuf,
        encoded: String,
        context: TakeoverContext,
        decided: tokio::sync::watch::Receiver<u64>,
    }
    fn fixture(generation: u64, keeper_interval_ms: Option<u64>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let session_dir = dir.path().to_owned();
        let terminal_dir = session_dir.join("extensions/terminal");
        let encoded = maho_core::session_sidecar_store::encoded_session_id("terminal-persistence");
        let decided = Arc::new(tokio::sync::watch::channel(0u64).0);
        let receiver = decided.subscribe();
        let digests: Digests = Arc::default();
        digests.lock().unwrap().generation = generation;
        let context = TakeoverContext {
            state: Arc::new(tokio::sync::Mutex::new(None)),
            recorded: Arc::default(),
            manager: Arc::new(Mutex::new(TerminalManager::default())),
            monitors: Arc::new(Mutex::new(crate::monitor_registry::MonitorRegistry::new(
                |_| {},
            ))),
            digests,
            terminal_dir: terminal_dir.clone(),
            encoded: encoded.clone(),
            shell: None,
            settings: crate::settings::TERMINAL_SETTINGS_DEFAULTS.clone(),
            generation,
            decided,
            keeper_interval_ms,
            restore_grace_ms: None,
            flush: None,
            restore_tasks: Arc::default(),
            grace_stop: Arc::new(tokio::sync::watch::channel(false).0),
            ui: None,
        };
        Fixture {
            #[cfg(unix)]
            dir,
            #[cfg(not(unix))]
            _dir: dir,
            session_dir,
            terminal_dir,
            encoded,
            context,
            decided: receiver,
        }
    }
    /// A manifest the previous generation left: one persistent command watch.
    async fn persisted_watch(fixture: &Fixture, monitor_id: &str) {
        persisted_command(fixture, monitor_id, "cat").await;
    }
    async fn persisted_command(fixture: &Fixture, monitor_id: &str, command: &str) {
        persisted_command_for(fixture, "terminal-persistence", monitor_id, command).await;
    }
    async fn persisted_command_for(
        fixture: &Fixture,
        session_id: &str,
        monitor_id: &str,
        command: &str,
    ) {
        let mut writer =
            crate::terminal_manifest::TerminalManifestWriter::new(&fixture.session_dir, session_id);
        writer
            .record_register(
                crate::terminal_manifest_model::MonitorRegistration {
                    monitor_id: monitor_id.to_owned(),
                    spec: crate::terminal_manifest_model::MonitorSpec::Command {
                        description: format!("watch {monitor_id}"),
                        command: command.into(),
                        filter: None,
                        human_input: None,
                        wakes_per_hour: None,
                        cwd: Some(fixture.session_dir.to_string_lossy().into_owned()),
                        persistent: true,
                    },
                },
                terminal_now_ms() - 60_000.0,
            )
            .await;
        writer.record_shutdown(terminal_now_ms() - 60_000.0).await;
    }
    async fn decided(fixture: &mut Fixture) {
        let generation = fixture.context.generation;
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            fixture.decided.wait_for(|decided| *decided >= generation),
        )
        .await
        .expect("restore decided in time")
        .expect("decision channel open");
    }
    fn teardown(fixture: &Fixture) {
        fixture.context.monitors.lock().unwrap().dispose();
        fixture.context.manager.lock().unwrap().teardown().unwrap();
    }

    /// restore-session `own(lease)`: the manifest is restored, the decided digest names every watch,
    /// the lifecycle is bound and the decision is signalled.
    #[tokio::test]
    async fn own_restores_the_manifest_sets_one_decided_digest_and_binds_the_lifecycle() {
        let mut fixture = fixture(1, None);
        persisted_watch(&fixture, "mon_OWNED0000000001").await;
        let mut lifecycle = crate::terminal_state::TerminalStateLifecycle::new(
            &fixture.session_dir,
            "terminal-persistence",
        );
        assert_eq!(
            lifecycle
                .acquire(
                    &fixture.terminal_dir,
                    &fixture.encoded,
                    f64::from(std::process::id()),
                    terminal_now_ms()
                )
                .unwrap(),
            crate::terminal_state::TerminalStateAdoption::Acquired
        );
        fixture.context.own(lifecycle, terminal_now_ms()).await;
        decided(&mut fixture).await;
        let pending = fixture
            .context
            .digests
            .lock()
            .unwrap()
            .slot
            .pending()
            .map(|message| message.details.clone())
            .expect("decided digest");
        assert_eq!(
            pending.outcome,
            crate::restore_digest::RestoreDigestOutcome::Decided
        );
        assert_eq!(pending.monitors.len(), 1);
        assert_eq!(pending.monitors[0].monitor_id, "mon_OWNED0000000001");
        assert_eq!(pending.monitors[0].outcome, "restored");
        assert!(
            fixture
                .context
                .state
                .lock()
                .await
                .as_ref()
                .is_some_and(|lifecycle| lifecycle.is_owner())
        );
        assert_eq!(fixture.context.monitors.lock().unwrap().snapshot().len(), 1);
        teardown(&fixture);
    }

    /// A takeover that lands after a newer generation started never overwrites that generation's digest.
    #[tokio::test]
    async fn own_from_a_stale_generation_sets_no_digest() {
        let fixture = fixture(1, None);
        // The script exits 0 at once, so the grace window settles it `completed`; a stale restore
        // must still neither forget it from the bound lifecycle nor rewrite the manifest.
        persisted_command(&fixture, "mon_STALEGEN00000001", "true").await;
        let mut lifecycle = crate::terminal_state::TerminalStateLifecycle::new(
            &fixture.session_dir,
            "terminal-persistence",
        );
        lifecycle
            .acquire(
                &fixture.terminal_dir,
                &fixture.encoded,
                f64::from(std::process::id()),
                terminal_now_ms(),
            )
            .unwrap();
        // `own` up to the detached grace window, inline so the test awaits `decide` itself.
        let restore_state = lifecycle.read_restore_state().await;
        let digest = {
            let mut manager = fixture.context.manager.lock().unwrap();
            let mut monitors = fixture.context.monitors.lock().unwrap();
            lifecycle.apply_restore(
                restore_state,
                &mut manager,
                &mut monitors,
                terminal_now_ms(),
                None,
                &fixture.context.settings,
            )
        };
        *fixture.context.state.lock().await = Some(lifecycle);
        // A newer generation started meanwhile: it owns the slot (its deferred note).
        fixture.context.digests.lock().unwrap().generation = 2;
        fixture
            .context
            .digests
            .lock()
            .unwrap()
            .slot
            .set(self::digest(
                crate::restore_digest::RestoreDigestOutcome::Deferred,
                2,
            ));
        let flushes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = flushes.clone();
        let mut context = fixture.context.clone();
        context.flush = Some(Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        // The grace window ends when the script exits (bounded by RESTORE_GRACE_MS).
        context.decide(digest, 0.0, true).await;
        assert_eq!(
            *fixture.decided.borrow(),
            0,
            "a stale restore signals nothing"
        );
        assert_eq!(flushes.load(std::sync::atomic::Ordering::SeqCst), 0);
        let slot = fixture
            .context
            .digests
            .lock()
            .unwrap()
            .slot
            .pending()
            .map(|message| (message.details.generation, message.details.outcome));
        assert_eq!(
            slot,
            Some((2, crate::restore_digest::RestoreDigestOutcome::Deferred)),
            "the newer generation's slot is untouched"
        );
        assert_eq!(
            manifest_ids(&fixture),
            ["mon_STALEGEN00000001"],
            "the stale restore rewrites no manifest"
        );
        assert_eq!(
            fixture
                .context
                .state
                .lock()
                .await
                .as_ref()
                .unwrap()
                .writer
                .monitor_ids(),
            ["mon_STALEGEN00000001"],
            "the bound lifecycle's entries are untouched"
        );
        teardown(&fixture);
    }

    /// restore-session `stopPersistence`: a shutdown inside the grace window awaits the restore in
    /// flight before the shutdown record. The still-running persistent watch stays in the recorded
    /// manifest with its state dir, and no digest (let alone a `lost` one) is sent afterwards.
    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_inside_the_grace_window_keeps_the_watch_and_sends_no_digest() {
        let mut fixture = fixture(1, None);
        persisted_watch(&fixture, "mon_GRACESHUTDOWN01").await;
        fixture.context.restore_grace_ms = Some(600_000);
        let flushes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = flushes.clone();
        fixture.context.flush = Some(Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let mut lifecycle = crate::terminal_state::TerminalStateLifecycle::new(
            &fixture.session_dir,
            "terminal-persistence",
        );
        lifecycle
            .acquire(
                &fixture.terminal_dir,
                &fixture.encoded,
                f64::from(std::process::id()),
                terminal_now_ms(),
            )
            .unwrap();
        fixture.context.own(lifecycle, terminal_now_ms()).await;
        let state_dir = crate::monitor_state_dir::monitor_state_dir(
            &fixture.terminal_dir,
            "mon_GRACESHUTDOWN01",
        );
        assert!(state_dir.is_dir(), "the restored watch has its state dir");
        // The SessionShutdown order: stop the restore in flight, then sync, record, release.
        stop_restores(
            &fixture.context.digests,
            &fixture.context.grace_stop,
            &fixture.context.restore_tasks,
        )
        .await;
        assert!(fixture.context.restore_tasks.lock().unwrap().is_empty());
        let mut lifecycle = fixture.context.state.lock().await.take().unwrap();
        let snapshot = fixture.context.monitors.lock().unwrap().snapshot();
        lifecycle
            .writer
            .observe_monitor_state(&snapshot, terminal_now_ms())
            .await
            .unwrap();
        lifecycle.record_shutdown(terminal_now_ms()).await;
        lifecycle.release().unwrap();
        // The manager goes down after the record (the bundle shutdown); the restore is already over,
        // so the dying watch is never judged.
        teardown(&fixture);
        assert_eq!(manifest_ids(&fixture), ["mon_GRACESHUTDOWN01"]);
        assert!(
            state_dir.is_dir(),
            "the watch's baseline survives the shutdown"
        );
        assert!(
            pending_monitors(&fixture).is_empty(),
            "no digest is decided for a generation that shut down"
        );
        assert_eq!(flushes.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(*fixture.decided.borrow_and_update(), 0);
    }

    /// terminal-restore-session.test.ts (g2): an unreadable manifest is left exactly as it was
    /// (fail closed) and the decided digest says `corrupt`.
    #[tokio::test]
    async fn an_unreadable_manifest_is_left_exactly_as_it_was() {
        let mut fixture = fixture(1, None);
        std::fs::create_dir_all(&fixture.terminal_dir).unwrap();
        let manifest = fixture
            .terminal_dir
            .join(format!("{}.json", fixture.encoded));
        std::fs::write(&manifest, "{not a manifest").unwrap();
        own_once(&mut fixture).await;
        let outcome = fixture
            .context
            .digests
            .lock()
            .unwrap()
            .slot
            .pending()
            .map(|message| message.details.outcome);
        assert_eq!(
            outcome,
            Some(crate::restore_digest::RestoreDigestOutcome::Corrupt)
        );
        assert_eq!(
            std::fs::read_to_string(&manifest).unwrap(),
            "{not a manifest"
        );
        teardown(&fixture);
    }

    /// terminal-restore-session.test.ts (i): a watch created while waiting on a foreign holder is
    /// queued (upstream `handMonitorSpec`), drained into the takeover's writer and on disk with the
    /// restored watch at shutdown.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_watch_created_while_waiting_is_on_disk_after_the_takeover() {
        // Its own session id: the pending-spec binding map is process-wide, and the other tests'
        // takeovers bind "terminal-persistence".
        let session = "terminal-waiter";
        let mut fixture = fixture(1, Some(10));
        fixture.encoded = maho_core::session_sidecar_store::encoded_session_id(session);
        fixture.context.encoded = fixture.encoded.clone();
        persisted_command_for(&fixture, session, "mon_WAITERBASE0001", "cat").await;
        std::fs::create_dir_all(&fixture.terminal_dir).unwrap();
        let lease = fixture
            .terminal_dir
            .join(format!("{}.lease", fixture.encoded));
        let holder = crate::manifest_lease::LeaseRecord::v1(
            f64::from(std::os::unix::process::parent_id()),
            0.0,
        );
        std::fs::write(&lease, serde_json::to_vec(&holder).unwrap()).unwrap();
        let ctx = context(fixture.dir.path(), Arc::new(RecordingUi::default()), None);
        let keeper: Arc<Mutex<Option<crate::lease_keeper::LeaseKeeper>>> = Arc::default();
        wait_for_lease(
            &ctx,
            &fixture.context,
            &keeper,
            Some(holder),
            fixture.session_dir.clone(),
            session.into(),
        );
        // Created while held: no writer is bound, so the spec waits for the takeover's writer.
        assert!(
            crate::tools::monitor_manifest_binding::hand_monitor_spec(
                Some(session),
                crate::terminal_manifest_model::MonitorRegistration {
                    monitor_id: "mon_CREATEDWHILEHLD".into(),
                    spec: crate::terminal_manifest_model::MonitorSpec::Command {
                        description: "created while held".into(),
                        command: "cat".into(),
                        filter: None,
                        human_input: None,
                        wakes_per_hour: None,
                        cwd: None,
                        persistent: true,
                    },
                },
            )
            .is_none()
        );
        std::fs::remove_file(&lease).unwrap();
        decided(&mut fixture).await;
        stop_waiting(&keeper).await;
        let mut lifecycle = fixture.context.state.lock().await.take().unwrap();
        crate::tools::monitor_manifest_binding::unbind_terminal_manifest_writer(session);
        lifecycle.record_shutdown(terminal_now_ms()).await;
        lifecycle.release().unwrap();
        let mut ids = manifest_ids(&fixture);
        ids.sort();
        assert_eq!(ids, ["mon_CREATEDWHILEHLD", "mon_WAITERBASE0001"]);
        teardown(&fixture);
    }

    /// 02a7938a0: `own` binds the writer and installs the lifecycle under one hold of the state
    /// lock. A monitor call that lands between the bind and the install (interleaved through the
    /// after-bind seam) must wait for the lifecycle and record through it; with the lock free at
    /// that instant it would see the session bound with no lifecycle and drop its spec.
    #[tokio::test]
    async fn a_monitor_call_between_bind_and_install_records_its_spec() {
        // Its own session id: the binding map and the after-bind seam are process-wide.
        let session = "terminal-bind-race";
        let mut fixture = fixture(1, None);
        fixture.encoded = maho_core::session_sidecar_store::encoded_session_id(session);
        fixture.context.encoded = fixture.encoded.clone();
        let mut lifecycle =
            crate::terminal_state::TerminalStateLifecycle::new(&fixture.session_dir, session);
        assert_eq!(
            lifecycle
                .acquire(
                    &fixture.terminal_dir,
                    &fixture.encoded,
                    f64::from(std::process::id()),
                    terminal_now_ms()
                )
                .unwrap(),
            crate::terminal_state::TerminalStateAdoption::Acquired
        );
        let registration = || crate::terminal_manifest_model::MonitorRegistration {
            monitor_id: "mon_BINDRACE00000001".into(),
            spec: crate::terminal_manifest_model::MonitorSpec::Command {
                description: "between bind and install".into(),
                command: "cat".into(),
                filter: None,
                human_input: None,
                wakes_per_hour: None,
                cwd: None,
                persistent: true,
            },
        };
        let handed: Arc<Mutex<Option<tokio::task::JoinHandle<bool>>>> = Arc::default();
        let hook_handed = handed.clone();
        let state = fixture.context.state.clone();
        crate::tools::monitor_manifest_binding::set_after_bind_hook(
            session,
            Box::new(move || {
                // The registered monitor tool's protocol: take the state lock, record through the
                // installed lifecycle, else hand the spec to the binding (dropped when bound).
                let task = match state.clone().try_lock_owned() {
                    Ok(slot) => {
                        let kept = slot.is_none()
                            && crate::tools::monitor_manifest_binding::hand_monitor_spec(
                                Some(session),
                                registration(),
                            )
                            .is_none();
                        tokio::spawn(async move { kept })
                    }
                    Err(_) => tokio::spawn(async move {
                        let mut slot = state.lock().await;
                        match slot.as_mut() {
                            Some(lifecycle) => {
                                lifecycle
                                    .writer
                                    .record_register(registration(), terminal_now_ms())
                                    .await;
                                true
                            }
                            None => crate::tools::monitor_manifest_binding::hand_monitor_spec(
                                Some(session),
                                registration(),
                            )
                            .is_none(),
                        }
                    }),
                };
                *hook_handed.lock().unwrap() = Some(task);
            }),
        );
        fixture.context.own(lifecycle, terminal_now_ms()).await;
        let task = handed
            .lock()
            .unwrap()
            .take()
            .expect("the monitor call ran between bind and install");
        assert!(
            task.await.unwrap(),
            "a spec handed off between bind and install is recorded, not dropped"
        );
        decided(&mut fixture).await;
        let mut lifecycle = fixture.context.state.lock().await.take().unwrap();
        crate::tools::monitor_manifest_binding::unbind_terminal_manifest_writer(session);
        lifecycle.record_shutdown(terminal_now_ms()).await;
        lifecycle.release().unwrap();
        assert_eq!(manifest_ids(&fixture), ["mon_BINDRACE00000001"]);
        teardown(&fixture);
    }

    /// extension-state.ts `bundleSinks.onMonitorEnded`: a watch that ended (exit, timeout, kill)
    /// loses its restore baseline dir; a disposed one (shutdown or reload) keeps it, and an event
    /// with no stable id or no durable state dir removes nothing.
    #[test]
    fn an_ended_watch_loses_its_state_dir_and_a_disposed_one_keeps_it() {
        use crate::monitor_registry::{MonitorEndedEvent, MonitorEndedReason};
        let dir = tempfile::tempdir().unwrap();
        let ui = Arc::new(RecordingUi::default());
        let ctx = context(dir.path(), ui.clone(), None);
        let terminal_dir = crate::monitor_state_dir::terminal_state_dir(Some(&ctx)).unwrap();
        let ended = |monitor_id: Option<&str>, reason| MonitorEndedEvent {
            id: "bash_1".into(),
            monitor_id: monitor_id.map(str::to_owned),
            description: "watch".into(),
            started_at_ms: 0.0,
            ended_at_ms: 1.0,
            reason,
            exit_code: None,
            fire_count: 0,
        };
        for (monitor_id, reason, kept) in [
            ("mon_DISPOSEDWATCH01", MonitorEndedReason::Disposed, true),
            ("mon_EXITEDWATCH001", MonitorEndedReason::Exit, false),
            ("mon_TIMEDOUTWATCH1", MonitorEndedReason::Timeout, false),
            ("mon_KILLEDWATCH001", MonitorEndedReason::Killed, false),
        ] {
            let state =
                crate::monitor_state_dir::ensure_monitor_state_dir_sync(&terminal_dir, monitor_id)
                    .unwrap();
            remove_ended_monitor_state_dir(&ended(Some(monitor_id), reason.clone()), Some(&ctx));
            assert_eq!(state.is_dir(), kept, "{reason:?}");
        }
        let unnamed = crate::monitor_state_dir::ensure_monitor_state_dir_sync(
            &terminal_dir,
            "mon_UNNAMED00001",
        )
        .unwrap();
        remove_ended_monitor_state_dir(&ended(None, MonitorEndedReason::Exit), Some(&ctx));
        remove_ended_monitor_state_dir(
            &ended(Some("mon_UNNAMED00001"), MonitorEndedReason::Exit),
            None,
        );
        assert!(unnamed.is_dir());
        assert!(ui.notes.lock().unwrap().is_empty());
    }

    /// The post-takeover sweep removes other sessions' stale leases and empty manifests and keeps
    /// this session's own lease and manifest.
    #[tokio::test]
    async fn sweep_after_takeover_removes_stale_state_and_keeps_our_own() {
        let fixture = fixture(1, None);
        std::fs::create_dir_all(&fixture.terminal_dir).unwrap();
        // Beyond any pid_max, so the probe answers ESRCH.
        let dead_pid = f64::from(i32::MAX);
        let write = |name: &str, content: &str| {
            std::fs::write(fixture.terminal_dir.join(name), content).unwrap()
        };
        write(
            "other.lease",
            &json!({"pid":dead_pid,"startedAtMs":1.0}).to_string(),
        );
        write("other.json", r#"{"monitors":[],"backgroundSessions":[]}"#);
        write(
            &format!("{}.lease", fixture.encoded),
            &json!({"pid":dead_pid,"startedAtMs":1.0}).to_string(),
        );
        write(
            &format!("{}.json", fixture.encoded),
            r#"{"monitors":[],"backgroundSessions":[]}"#,
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            sweep_terminal_state(&fixture.terminal_dir, &fixture.encoded),
        )
        .await
        .expect("sweep finished in time")
        .expect("sweep task");
        assert!(!fixture.terminal_dir.join("other.lease").exists());
        assert!(!fixture.terminal_dir.join("other.json").exists());
        assert!(
            fixture
                .terminal_dir
                .join(format!("{}.lease", fixture.encoded))
                .exists()
        );
        assert!(
            fixture
                .terminal_dir
                .join(format!("{}.json", fixture.encoded))
                .exists()
        );
    }

    /// terminal-restore-session.test.ts (b) and (j): a live holder defers the restore (status + a
    /// deferred digest); once the holder's lease is gone the keeper takes over and restores exactly
    /// once, replacing the deferred note with the decided digest.
    #[cfg(unix)]
    #[tokio::test]
    async fn wait_defers_then_the_keeper_takes_over_and_restores_once() {
        let mut fixture = fixture(1, Some(10));
        persisted_watch(&fixture, "mon_TAKEOVER00000001").await;
        std::fs::create_dir_all(&fixture.terminal_dir).unwrap();
        let lease = fixture
            .terminal_dir
            .join(format!("{}.lease", fixture.encoded));
        // A live foreign holder: our parent process stands in for it.
        let holder = crate::manifest_lease::LeaseRecord::v1(
            f64::from(std::os::unix::process::parent_id()),
            0.0,
        );
        std::fs::write(&lease, serde_json::to_vec(&holder).unwrap()).unwrap();
        let ui = Arc::new(RecordingUi::default());
        let ctx = context(fixture.dir.path(), ui.clone(), None);
        let keeper: Arc<Mutex<Option<crate::lease_keeper::LeaseKeeper>>> = Arc::default();
        wait_for_lease(
            &ctx,
            &fixture.context,
            &keeper,
            Some(holder.clone()),
            fixture.session_dir.clone(),
            "terminal-persistence".into(),
        );
        assert!(
            ui.statuses
                .lock()
                .unwrap()
                .iter()
                .any(|(key, text)| key == RESTORE_STATUS_KEY
                    && text.as_deref()
                        == Some(format!("monitors held by pid {}", holder.pid as u32).as_str()))
        );
        assert_eq!(
            fixture
                .context
                .digests
                .lock()
                .unwrap()
                .slot
                .pending()
                .map(|message| message.details.outcome),
            Some(crate::restore_digest::RestoreDigestOutcome::Deferred)
        );
        assert!(
            fixture.context.state.lock().await.is_none(),
            "nothing is restored while the holder lives"
        );

        // The holder goes away: the keeper acquires, takes over and restores.
        std::fs::remove_file(&lease).unwrap();
        decided(&mut fixture).await;
        let pending = fixture
            .context
            .digests
            .lock()
            .unwrap()
            .slot
            .pending()
            .map(|message| message.details.clone())
            .expect("decided digest");
        assert_eq!(
            pending.outcome,
            crate::restore_digest::RestoreDigestOutcome::Decided
        );
        assert_eq!(pending.monitors[0].monitor_id, "mon_TAKEOVER00000001");
        assert!(
            ui.statuses
                .lock()
                .unwrap()
                .last()
                .is_some_and(|(key, text)| key == RESTORE_STATUS_KEY && text.is_none())
        );
        let lease_record: crate::manifest_lease::LeaseRecord =
            serde_json::from_slice(&std::fs::read(&lease).unwrap()).unwrap();
        assert_eq!(
            lease_record.pid,
            f64::from(std::process::id()),
            "the takeover holds the lease"
        );
        assert_eq!(
            fixture.context.monitors.lock().unwrap().snapshot().len(),
            1,
            "restored exactly once"
        );
        stop_waiting(&keeper).await;
        teardown(&fixture);
    }

    /// restore-session `stopPersistence`: stopping while still waiting takes nothing over, and a
    /// later disappearance of the holder's lease is not acted on.
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_while_waiting_takes_nothing_over() {
        let fixture = fixture(1, Some(10));
        persisted_watch(&fixture, "mon_STOPPED00000001").await;
        std::fs::create_dir_all(&fixture.terminal_dir).unwrap();
        let lease = fixture
            .terminal_dir
            .join(format!("{}.lease", fixture.encoded));
        let holder = crate::manifest_lease::LeaseRecord::v1(
            f64::from(std::os::unix::process::parent_id()),
            0.0,
        );
        std::fs::write(&lease, serde_json::to_vec(&holder).unwrap()).unwrap();
        let ctx = context(fixture.dir.path(), Arc::new(RecordingUi::default()), None);
        let keeper: Arc<Mutex<Option<crate::lease_keeper::LeaseKeeper>>> = Arc::default();
        wait_for_lease(
            &ctx,
            &fixture.context,
            &keeper,
            Some(holder),
            fixture.session_dir.clone(),
            "terminal-persistence".into(),
        );
        let waiting = keeper.lock().unwrap().clone().expect("keeper armed");
        stop_waiting(&keeper).await;
        assert!(keeper.lock().unwrap().is_none());
        assert_eq!(
            waiting.state(),
            crate::lease_keeper::LeaseKeeperState::Stopped
        );
        std::fs::remove_file(&lease).unwrap();
        // The stopped keeper is settled: no tick is in flight and none is armed.
        waiting.settled().await;
        assert_eq!(
            waiting.state(),
            crate::lease_keeper::LeaseKeeperState::Stopped
        );
        assert!(!lease.exists(), "a stopped keeper never acquires");
        assert!(fixture.context.state.lock().await.is_none());
        teardown(&fixture);
    }

    #[cfg(unix)]
    fn pending_monitors(fixture: &Fixture) -> Vec<crate::restore_digest::MonitorRestoreResult> {
        fixture
            .context
            .digests
            .lock()
            .unwrap()
            .slot
            .pending()
            .map(|message| message.details.monitors.clone())
            .unwrap_or_default()
    }
    fn manifest_ids(fixture: &Fixture) -> Vec<String> {
        let path = fixture
            .terminal_dir
            .join(format!("{}.json", fixture.encoded));
        let Ok(raw) = std::fs::read(&path) else {
            return vec![];
        };
        let saved: JsonValue = serde_json::from_slice(&raw).unwrap();
        saved["monitors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["monitorId"].as_str().unwrap().to_owned())
            .collect()
    }
    async fn own_once(fixture: &mut Fixture) {
        let mut lifecycle = crate::terminal_state::TerminalStateLifecycle::new(
            &fixture.session_dir,
            "terminal-persistence",
        );
        assert_eq!(
            lifecycle
                .acquire(
                    &fixture.terminal_dir,
                    &fixture.encoded,
                    f64::from(std::process::id()),
                    terminal_now_ms()
                )
                .unwrap(),
            crate::terminal_state::TerminalStateAdoption::Acquired
        );
        fixture.context.own(lifecycle, terminal_now_ms()).await;
        decided(fixture).await;
    }

    /// terminal-restore-session.test.ts (c): a lease whose pid now belongs to a process that
    /// started long after the lease was taken is reclaimed at once - no "held by" wait.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_lease_held_by_a_reused_pid_is_reclaimed_at_session_start() {
        let mut fixture = fixture(1, Some(10));
        persisted_watch(&fixture, "mon_PIDREUSED000001").await;
        std::fs::create_dir_all(&fixture.terminal_dir).unwrap();
        // A live unrelated process (our parent) whose lease claims it was taken an hour before
        // that process actually started: the pid was reused.
        let pid = f64::from(std::os::unix::process::parent_id());
        let real_start =
            crate::manifest_lease::read_process_start_ms(pid).expect("parent start instant");
        std::fs::write(
            fixture
                .terminal_dir
                .join(format!("{}.lease", fixture.encoded)),
            json!({"pid":pid,"startedAtMs":real_start - 3_600_000.0}).to_string(),
        )
        .unwrap();
        let ui = Arc::new(RecordingUi::default());
        let ctx = context(fixture.dir.path(), ui.clone(), None);
        let keeper: Arc<Mutex<Option<crate::lease_keeper::LeaseKeeper>>> = Arc::default();
        acquire_or_wait(
            &ctx,
            &fixture.context,
            &keeper,
            crate::terminal_state::TerminalStateLifecycle::new(
                &fixture.session_dir,
                "terminal-persistence",
            ),
            fixture.session_dir.clone(),
            "terminal-persistence".into(),
            terminal_now_ms(),
        )
        .await;
        assert!(
            !ui.statuses
                .lock()
                .unwrap()
                .iter()
                .any(|(_, text)| text.as_deref().is_some_and(|text| text.contains("held by"))),
            "a reused pid is not a holder"
        );
        assert!(keeper.lock().unwrap().is_none(), "nothing waits on it");
        decided(&mut fixture).await;
        let monitors = pending_monitors(&fixture);
        assert_eq!(monitors.len(), 1);
        assert_eq!(monitors[0].monitor_id, "mon_PIDREUSED000001");
        assert_eq!(monitors[0].outcome, "restored");
        stop_waiting(&keeper).await;
        teardown(&fixture);
    }

    /// terminal-restore-session.test.ts (a3): a watch that exits 0 inside the grace window is
    /// `completed`, is dropped from the manifest at once, and a second start never re-runs it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_completed_watch_is_gone_from_the_manifest_and_never_rerun() {
        let mut fixture = fixture(1, None);
        let runs = fixture.dir.path().join("runs.log");
        persisted_command(
            &fixture,
            "mon_COMPLETEDONCE01",
            &format!("echo run >> '{}'", runs.display()),
        )
        .await;
        own_once(&mut fixture).await;
        let monitors = pending_monitors(&fixture);
        assert_eq!(
            monitors
                .iter()
                .map(|entry| entry.outcome.as_str())
                .collect::<Vec<_>>(),
            ["completed"]
        );
        assert!(
            manifest_ids(&fixture).is_empty(),
            "the completed watch is gone from disk before shutdown"
        );
        let mut lifecycle = fixture.context.state.lock().await.take().unwrap();
        lifecycle.record_shutdown(terminal_now_ms()).await;
        lifecycle.release().unwrap();
        teardown(&fixture);

        // The next process: nothing to restore, and the command never ran again.
        let mut second = fixture;
        second.context.generation = 2;
        second.context.digests.lock().unwrap().generation = 2;
        second.context.digests.lock().unwrap().slot = crate::restore_digest::create_digest_slot();
        second.context.manager = Arc::new(Mutex::new(TerminalManager::default()));
        second.context.monitors = Arc::new(Mutex::new(
            crate::monitor_registry::MonitorRegistry::new(|_| {}),
        ));
        own_once(&mut second).await;
        assert!(pending_monitors(&second).is_empty());
        assert_eq!(
            std::fs::read_to_string(&runs)
                .unwrap()
                .trim()
                .lines()
                .collect::<Vec<_>>(),
            ["run"]
        );
        teardown(&second);
    }

    /// terminal-restore-session.test.ts (d): a watch whose script is gone is lost with its non-zero
    /// exit code and first output line; lost and expired state dirs are removed.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_lost_script_carries_its_exit_reason_and_lost_and_expired_state_dirs_go() {
        let mut fixture = fixture(1, None);
        let missing = fixture.dir.path().join("missing.sh");
        persisted_command(
            &fixture,
            "mon_MISSINGSCRIPT01",
            &format!("sh {}", missing.display()),
        )
        .await;
        // An expired watch next to it: same manifest, deadline already passed.
        let manifest = fixture
            .terminal_dir
            .join(format!("{}.json", fixture.encoded));
        let mut saved: JsonValue =
            serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        let mut expired = saved["monitors"][0].clone();
        expired["monitorId"] = json!("mon_EXPIREDWATCH001");
        expired["command"] = json!("cat");
        expired["expiresAt"] = json!(terminal_now_ms() - 1_000.0);
        saved["monitors"].as_array_mut().unwrap().push(expired);
        std::fs::write(&manifest, saved.to_string()).unwrap();
        for monitor_id in ["mon_MISSINGSCRIPT01", "mon_EXPIREDWATCH001"] {
            let dir =
                crate::monitor_state_dir::monitor_state_dir(&fixture.terminal_dir, monitor_id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("base"), "baseline").unwrap();
        }
        own_once(&mut fixture).await;
        let monitors = pending_monitors(&fixture);
        assert_eq!(
            monitors
                .iter()
                .map(|entry| (entry.monitor_id.as_str(), entry.outcome.as_str()))
                .collect::<Vec<_>>(),
            [
                ("mon_MISSINGSCRIPT01", "lost"),
                ("mon_EXPIREDWATCH001", "expired")
            ]
        );
        let reason = monitors[0].reason.clone().expect("lost reason");
        assert!(
            regex::Regex::new(r"^exited [1-9]\d* in \d+ms: .*missing\.sh")
                .unwrap()
                .is_match(&reason),
            "{reason}"
        );
        for monitor_id in ["mon_MISSINGSCRIPT01", "mon_EXPIREDWATCH001"] {
            assert!(
                !crate::monitor_state_dir::monitor_state_dir(&fixture.terminal_dir, monitor_id)
                    .exists(),
                "{monitor_id} state dir removed"
            );
        }
        assert!(manifest_ids(&fixture).is_empty());
        teardown(&fixture);
    }

    /// terminal-resumption-channel-state.test.ts: the wake-source channel names a monitor by its
    /// stable `mon_` id, never the runtime `bash_N` id (row 42).
    #[tokio::test]
    async fn wake_source_state_names_monitors_by_their_stable_mon_id() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _) = sender(dir.path());
        let (states, mut received) = tokio::sync::mpsc::unbounded_channel();
        let _subscription = api.events.on(
            "wake_source_state",
            Arc::new(move |state| {
                if state["source"] == "terminal-monitors" {
                    states.send(state.clone()).unwrap();
                }
            }),
        );
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let task = bind_monitor_events(
            registry.subscribe_state(),
            api,
            Arc::new(tokio::sync::Mutex::new(None)),
        );
        let (id, monitor_id) = registry
            .register_persistent_file(
                "dual emit monitor",
                &dir.path().join("file"),
                crate::terminal_manifest_model::FileEvent::Create,
            )
            .unwrap();
        assert!(monitor_id.starts_with("mon_"));
        assert_ne!(id, monitor_id);
        let state = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let state = received.recv().await.unwrap();
                if state["activeCount"] == 1 {
                    return state;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(state["monitors"].as_array().unwrap().len(), 1);
        assert_eq!(state["monitors"][0]["id"], monitor_id);
        assert_eq!(state["monitors"][0]["description"], "dual emit monitor");
        assert!(state["monitors"][0]["startedAtMs"].is_number());
        registry.dispose();
        task.abort();
    }

    #[tokio::test]
    async fn orphan_kill_failure_aborts_restore_and_marks_digest_corrupt() {
        let mut fixture = fixture(1, None);
        let ui = Arc::new(RecordingUi::default());
        fixture.context.ui = Some(ui.clone());
        std::fs::create_dir_all(&fixture.terminal_dir).unwrap();
        let runtime = crate::process_identity::ChildProcessIdentity {
            pid: 4242,
            process_group_id: Some(4242),
            started_at_ms: terminal_now_ms() - 60_000.0,
            boot_at_ms: 1.0,
            argv: vec!["cat".to_owned()],
        };
        let mut writer = crate::terminal_manifest::TerminalManifestWriter::new(
            &fixture.session_dir,
            "terminal-persistence",
        );
        writer
            .record_register_with(
                crate::terminal_manifest_model::MonitorRegistration {
                    monitor_id: "mon_ORPHANFAIL".to_owned(),
                    spec: crate::terminal_manifest_model::MonitorSpec::Command {
                        description: "watch mon_ORPHANFAIL".into(),
                        command: "cat".into(),
                        filter: None,
                        human_input: None,
                        wakes_per_hour: None,
                        cwd: Some(fixture.session_dir.to_string_lossy().into_owned()),
                        persistent: true,
                    },
                }
                .with_deadline_ms(None)
                .with_runtime(Some(runtime)),
                terminal_now_ms() - 60_000.0,
            )
            .await;
        writer.record_shutdown(terminal_now_ms() - 60_000.0).await;
        let manifest_path = fixture
            .terminal_dir
            .join(format!("{}.json", fixture.encoded));
        let before_manifest = std::fs::read_to_string(&manifest_path).unwrap();

        let mut lifecycle = crate::terminal_state::TerminalStateLifecycle::new(
            &fixture.session_dir,
            "terminal-persistence",
        );
        lifecycle.adopt_lease(crate::terminal_state::HeldLease {
            path: fixture
                .terminal_dir
                .join(format!("{}.lease", fixture.encoded)),
            pid: f64::from(std::process::id()),
            token: "test-token".to_owned(),
        });
        lifecycle.writer.reap_orphan = Some(std::sync::Arc::new(|_, _| {
            Box::pin(async {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "kill tree failed: operation not permitted",
                ))
            })
        }));

        fixture.context.own(lifecycle, terminal_now_ms()).await;
        decided(&mut fixture).await;

        let outcome = fixture
            .context
            .digests
            .lock()
            .unwrap()
            .slot
            .pending()
            .map(|message| message.details.outcome);
        assert_eq!(
            outcome,
            Some(crate::restore_digest::RestoreDigestOutcome::Corrupt)
        );
        assert_eq!(
            std::fs::read_to_string(&manifest_path).unwrap(),
            before_manifest
        );
        assert_eq!(fixture.context.manager.lock().unwrap().size(), 0);
        assert!(fixture.context.state.lock().await.is_some());
        assert!(
            ui.notes
                .lock()
                .unwrap()
                .iter()
                .any(|note| note.contains("Terminal restore failed"))
        );

        teardown(&fixture);
    }
}
