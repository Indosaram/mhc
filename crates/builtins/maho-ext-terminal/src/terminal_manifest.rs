use crate::terminal_manifest_model::*;
use indexmap::IndexMap;
use maho_core::session_sidecar_store::{
    CreateSidecarStoreOptions, SidecarError, SidecarStore, create_sidecar_store,
};
use std::collections::BTreeMap;

pub fn create_terminal_manifest_store(
    session_dir: &std::path::Path,
    session_id: &str,
) -> SidecarStore {
    create_sidecar_store(CreateSidecarStoreOptions {
        base_dir: session_dir
            .join("extensions/terminal")
            .to_string_lossy()
            .into_owned(),
        session_id: session_id.to_owned(),
        version: i64::from(TERMINAL_MANIFEST_VERSION),
        temp_prefix: "terminal".to_owned(),
        parse: std::sync::Arc::new(|value, reference| {
            let manifest = crate::restore::parse_terminal_manifest(value, &reference.session_id)
                .map_err(|error| SidecarError::Invalid(error.to_string()))?;
            serde_json::to_value(manifest).map_err(|error| SidecarError::Invalid(error.to_string()))
        }),
    })
}
pub struct TerminalManifestWriter {
    pub store: SidecarStore,
    session_id: String,
    entries: IndexMap<String, ManifestMonitor>,
    backgrounds: IndexMap<String, ManifestBackgroundSession>,
    pending: BTreeMap<String, TerminalManifestCheckpoint>,
    /// Shared so a live rpc watch can checkpoint without the writer lock (`rpc_checkpoint_sink`).
    pending_rpc: std::sync::Arc<std::sync::Mutex<BTreeMap<String, TerminalManifestRpcCheckpoint>>>,
    pub persist_failure: Option<SidecarError>,
    /// Where a restored rpc watch looks for endpoints (upstream `CheckpointedRpcRestoreDeps.agentDir`);
    /// `None` is `getAgentDir()`.
    pub rpc_agent_dir: Option<std::path::PathBuf>,
    /// Orphans found by `reap_orphans`, attached to the next restore's results by monitor id.
    reaped: BTreeMap<String, crate::restore_digest::Orphan>,
    /// Test seam for `reapBeforeRespawn` (upstream `RestartableCommandDeps.reapOrphan`).
    pub reap_orphan: Option<ReapOrphan>,
}
/// `reapBeforeRespawn(runtime, monitorId)`.
pub type ReapOrphan = std::sync::Arc<
    dyn Fn(
            crate::process_identity::ChildProcessIdentity,
            String,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = std::io::Result<crate::orphan_reaper::ReapResult>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;
impl TerminalManifestWriter {
    pub fn new(session_dir: &std::path::Path, session_id: &str) -> Self {
        Self {
            store: create_terminal_manifest_store(session_dir, session_id),
            session_id: session_id.to_owned(),
            entries: IndexMap::new(),
            backgrounds: IndexMap::new(),
            pending: BTreeMap::new(),
            pending_rpc: Default::default(),
            persist_failure: None,
            rpc_agent_dir: None,
            reaped: BTreeMap::new(),
            reap_orphan: None,
        }
    }
    /// Upstream restartable-command `reapOrphan` + `orphanOf`, run before the synchronous apply: for
    /// every command watch the restore will re-spawn that recorded its child, confirm the old
    /// watcher still runs and stop its tree. A kill reports `killed`, an unconfirmable pid
    /// `unverified`, a dead one nothing.
    pub async fn reap_orphans(&mut self, state: &TerminalManifest, now: f64) {
        self.reaped.clear();
        for monitor in &state.monitors {
            let Some(runtime) = &monitor.runtime else {
                continue;
            };
            let respawned = monitor.runtime_kind == MonitorRuntimeKind::Command
                && monitor.expires_at.is_none_or(|expiry| expiry > now)
                && (monitor.persistent
                    || monitor.deadline_ms.is_some_and(|deadline| deadline > now));
            if !respawned {
                continue;
            }
            let reaped = match &self.reap_orphan {
                Some(reap) => reap(runtime.clone(), monitor.monitor_id.clone()).await,
                None => {
                    crate::orphan_reaper::reap_before_respawn(
                        runtime,
                        &monitor.monitor_id,
                        &crate::orphan_reaper::OwnerProbes::default(),
                        &crate::orphan_reaper::KillTreeOptions::default(),
                    )
                    .await
                }
            };
            let action = match reaped {
                Ok(result) if result.action == crate::orphan_reaper::ReapAction::Killed => {
                    Some(crate::restore_digest::OrphanAction::Killed)
                }
                Ok(result) if result.reason.is_some() => {
                    Some(crate::restore_digest::OrphanAction::Unverified)
                }
                Ok(_) => None,
                // Native divergence: upstream lets a kill error fail the whole restore; here the
                // watch is reported with an unverified orphan and the restore goes on.
                Err(error) => {
                    maho_ext_api::diagnostic(&format!(
                        "terminal restore: stopping the previous watcher of {} failed: {error}",
                        monitor.monitor_id
                    ));
                    Some(crate::restore_digest::OrphanAction::Unverified)
                }
            };
            if let Some(action) = action {
                self.reaped.insert(
                    monitor.monitor_id.clone(),
                    crate::restore_digest::Orphan {
                        pid: runtime.pid,
                        action,
                    },
                );
            }
        }
    }
    pub async fn record_register(&mut self, registration: MonitorRegistration, now: f64) {
        self.record_register_with(registration.into(), now).await;
    }
    /// `recordRegister` with the registration's optional ephemeral `deadlineMs`.
    pub async fn record_register_with(&mut self, recorded: RecordedRegistration, now: f64) {
        let RecordedRegistration {
            registration,
            deadline_ms,
            runtime,
        } = recorded;
        let (
            description,
            runtime_kind,
            command,
            path,
            event,
            filter,
            human_input,
            wakes_per_hour,
            cwd,
            approved_parent,
            persistent,
            rpc,
        ) = match registration.spec {
            MonitorSpec::Command {
                description,
                command,
                filter,
                human_input,
                wakes_per_hour,
                cwd,
                persistent,
            } => (
                description,
                MonitorRuntimeKind::Command,
                Some(command),
                None,
                None,
                filter,
                human_input.filter(|human_input| *human_input),
                wakes_per_hour,
                cwd,
                None,
                persistent,
                None,
            ),
            MonitorSpec::File {
                description,
                path,
                event,
                cwd,
                approved_parent,
                persistent,
                ..
            } => (
                description,
                MonitorRuntimeKind::File,
                None,
                Some(path),
                Some(event),
                None,
                None,
                None,
                Some(cwd),
                approved_parent,
                persistent,
                None,
            ),
            MonitorSpec::Rpc {
                description,
                target,
                events,
                exclude,
                filter,
                persistent,
            } => (
                description,
                MonitorRuntimeKind::Rpc,
                None,
                None,
                None,
                filter,
                None,
                None,
                None,
                None,
                persistent,
                Some((target, events, exclude)),
            ),
        };
        let durability_class = if !persistent {
            MonitorDurabilityClass::Ephemeral
        } else if runtime_kind == MonitorRuntimeKind::File {
            MonitorDurabilityClass::CheckpointedFile
        } else if runtime_kind == MonitorRuntimeKind::Rpc {
            MonitorDurabilityClass::CheckpointedRpc
        } else {
            MonitorDurabilityClass::RestartableCommand
        };
        let (target, events, exclude) = rpc.unwrap_or_default();
        self.entries.insert(
            registration.monitor_id.clone(),
            ManifestMonitor {
                monitor_id: registration.monitor_id,
                session_id: self.session_id.clone(),
                description,
                runtime_kind,
                durability_class,
                command,
                path,
                event,
                filter,
                human_input,
                wakes_per_hour,
                cwd,
                approved_parent,
                target: (!target.is_empty()).then_some(target),
                events,
                exclude,
                rpc_checkpoint: None,
                created_at: now,
                expires_at: persistent.then_some(now + DURABLE_MONITOR_EXPIRY_MS as f64),
                persistent,
                suspended: false,
                last_checkpoint: None,
                delivery_paused: false,
                fire_window: ManifestFireWindow {
                    start_ms: now,
                    count: 0.0,
                },
                runtime: runtime.filter(|_| runtime_kind == MonitorRuntimeKind::Command),
                deadline_ms: deadline_ms.filter(|_| !persistent),
            },
        );
        self.persist(now).await;
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    /// Upstream SF-2 `seedFromDisk`: a reload generation's fresh writer starts from the manifest
    /// the previous generation wrote (its unsuspended entries and background sessions), so the
    /// first post-reload write keeps the pre-reload watches. An unreadable manifest seeds nothing.
    pub async fn seed_from_disk(&mut self) {
        let Ok(Some(state)) = self.read_restore_state().await else {
            return;
        };
        for entry in state.monitors {
            if !entry.suspended {
                self.entries.insert(entry.monitor_id.clone(), entry);
            }
        }
        for background in state.background_sessions {
            self.backgrounds.insert(background.id.clone(), background);
        }
    }
    /// The session's terminal state dir (`<sessionDir>/extensions/terminal`) the store lives in.
    pub fn terminal_dir(&self) -> Option<&std::path::Path> {
        std::path::Path::new(self.store.file_path()).parent()
    }
    pub fn adopt_restored(&mut self, mut entry: ManifestMonitor) {
        entry.suspended = false;
        self.entries.insert(entry.monitor_id.clone(), entry);
    }
    /// Drops a re-adopted entry whose restored command ended inside the grace window (upstream
    /// adopts only a watch still live after the grace), so the next persist leaves it out.
    pub fn forget_restored(&mut self, monitor_id: &str) {
        self.entries.shift_remove(monitor_id);
    }
    /// Upstream `persistRestored`: rewrite a readable manifest to the restore's truth at once, so a
    /// watch that did not come back is never re-run by a later start.
    pub async fn persist_restored(&mut self, now: f64) {
        self.persist(now).await;
    }
    /// The stable ids of the recorded entries, in order.
    pub fn monitor_ids(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }
    pub fn durable_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| entry.durability_class != MonitorDurabilityClass::Ephemeral)
            .count()
    }
    pub async fn restore_live(
        &mut self,
        manager: &mut crate::manager::TerminalManager,
        registry: &mut crate::monitor_registry::MonitorRegistry,
        now: f64,
    ) -> crate::restore::RestoreDigest {
        self.restore_configured_live(
            manager,
            registry,
            now,
            None,
            &crate::settings::TERMINAL_SETTINGS_DEFAULTS,
        )
        .await
    }
    /// Reads and parses the persisted manifest with no locks held, so a caller that owns the
    /// manager/registry mutexes can acquire them only for the synchronous apply below (a
    /// `std::sync::MutexGuard` must never be held across this await).
    pub async fn read_restore_state(
        &self,
    ) -> std::result::Result<Option<crate::terminal_manifest_model::TerminalManifest>, ()> {
        match self.store.read().await {
            Ok(None) => Ok(None),
            Ok(Some(value)) => {
                match crate::restore::parse_terminal_manifest(&value, &self.session_id) {
                    Ok(state) => Ok(Some(state)),
                    Err(_) => Err(()),
                }
            }
            Err(_) => Err(()),
        }
    }
    pub async fn restore_configured_live(
        &mut self,
        manager: &mut crate::manager::TerminalManager,
        registry: &mut crate::monitor_registry::MonitorRegistry,
        now: f64,
        shell: Option<&str>,
        settings: &crate::settings::ResolvedTerminalSettings,
    ) -> crate::restore::RestoreDigest {
        let state = self.read_restore_state().await;
        if let Ok(Some(manifest)) = &state {
            self.reap_orphans(manifest, now).await;
        }
        self.apply_restore_state(state, manager, registry, now, shell, settings)
    }
    pub fn apply_restore_state(
        &mut self,
        state: std::result::Result<Option<crate::terminal_manifest_model::TerminalManifest>, ()>,
        manager: &mut crate::manager::TerminalManager,
        registry: &mut crate::monitor_registry::MonitorRegistry,
        now: f64,
        shell: Option<&str>,
        settings: &crate::settings::ResolvedTerminalSettings,
    ) -> crate::restore::RestoreDigest {
        use crate::restore::{RestoreDigest, RestoreOutcome};
        let mut digest = RestoreDigest::default();
        let state = match state {
            Ok(None) => return digest,
            Ok(Some(state)) => state,
            Err(()) => {
                digest.store_error = true;
                return digest;
            }
        };
        // Upstream `restoreTerminalState`: the downtime is an upper bound measured from the
        // manifest's last write, never an exact gap.
        let downtime_ms = (now - state.updated_at).max(0.0);
        let terminal_dir = self.terminal_dir().map(std::path::Path::to_path_buf);
        let restored =
            |monitor: &ManifestMonitor, outcome: &'static str| crate::restore::RestoredMonitor {
                monitor_id: monitor.monitor_id.clone(),
                description: monitor.description.clone(),
                file: monitor.runtime_kind
                    == crate::terminal_manifest_model::MonitorRuntimeKind::File,
                outcome,
                command: monitor.command.clone(),
                path: monitor.path.clone(),
                reason: None,
                orphan: None,
            };
        for monitor in state.monitors {
            if monitor.expires_at.is_some_and(|expiry| expiry <= now) {
                digest.expired += 1;
                digest.results.push(restored(&monitor, "expired"));
                continue;
            }
            // An ephemeral watch whose runtime deadline has not passed comes back with exactly the
            // time it had left (upstream `RestoreContext.remainingMs`); otherwise it is lost.
            let remaining_ms = monitor
                .deadline_ms
                .filter(|deadline| *deadline > now)
                .map(|deadline| deadline - now);
            let restore_environment = crate::durable_command::RestoreEnvironment {
                downtime_ms,
                terminal_dir: terminal_dir.clone(),
                remaining_ms,
            };
            let mut reason = None;
            let outcome = match monitor.durability_class {
                MonitorDurabilityClass::CheckpointedRpc => {
                    let restored = self.restore_rpc(&monitor, None, manager, registry);
                    reason = restored.reason;
                    restored.outcome
                }
                MonitorDurabilityClass::Ephemeral
                    if remaining_ms.is_some()
                        && monitor.runtime_kind == MonitorRuntimeKind::Rpc =>
                {
                    let restored = self.restore_rpc(&monitor, remaining_ms, manager, registry);
                    reason = restored.reason;
                    restored.outcome
                }
                MonitorDurabilityClass::Ephemeral
                    if remaining_ms.is_some()
                        && monitor.runtime_kind == MonitorRuntimeKind::Command =>
                {
                    crate::durable_command::restore_configured_command_in(
                        &monitor,
                        manager,
                        shell,
                        settings,
                        &restore_environment,
                        |_, runtime, record| registry.register(runtime, record),
                    )
                }
                MonitorDurabilityClass::Ephemeral if remaining_ms.is_some() => {
                    crate::durable_file::restore_file_in(
                        &monitor,
                        registry,
                        manager,
                        None,
                        now,
                        remaining_ms,
                    )
                }
                MonitorDurabilityClass::Ephemeral => RestoreOutcome::Lost,
                MonitorDurabilityClass::RestartableCommand => {
                    crate::durable_command::restore_configured_command_in(
                        &monitor,
                        manager,
                        shell,
                        settings,
                        &restore_environment,
                        |_, runtime, record| registry.register(runtime, record),
                    )
                }
                MonitorDurabilityClass::CheckpointedFile => {
                    crate::durable_file::restore_file(&monitor, registry, manager, None, now)
                }
            };
            match outcome {
                RestoreOutcome::Restored => digest.restored += 1,
                RestoreOutcome::Muted => digest.muted += 1,
                RestoreOutcome::Lost => digest.lost += 1,
                RestoreOutcome::AttachedElsewhere => digest.attached_elsewhere += 1,
            }
            let mut result = restored(&monitor, outcome.bucket());
            result.reason = reason;
            result.orphan = self.reaped.remove(&monitor.monitor_id);
            digest.results.push(result);
            if matches!(outcome, RestoreOutcome::Restored | RestoreOutcome::Muted) {
                // Upstream adopts the entry with the fresh runtime identity, so the NEXT crash
                // stops this process instead of starting a copy.
                let mut adopted = monitor.clone();
                if monitor.runtime_kind == MonitorRuntimeKind::Command
                    && let Some(id) = manager.resolve_id(&monitor.monitor_id)
                {
                    adopted.runtime = manager.get(&id).and_then(|runtime| runtime.identity());
                }
                self.adopt_restored(adopted);
                registry.adopt_fire_window(&monitor.monitor_id, &monitor.fire_window);
                if let Some(id) = manager.resolve_id(&monitor.monitor_id)
                    && let Some(checkpoint) = registry.file_checkpoint(&id)
                {
                    self.schedule_checkpoint(&monitor.monitor_id, checkpoint);
                }
            }
        }
        digest.lost += state.background_sessions.len();
        digest.background_sessions = state
            .background_sessions
            .iter()
            .map(|session| crate::restore::RestoredBackground {
                id: session.id.clone(),
                command: session.command.clone(),
            })
            .collect();
        digest
    }
    /// The `checkpointed-rpc` handler (`durable_rpc`): the watch is armed under the registry now
    /// and its endpoint join runs on the runtime.
    fn restore_rpc(
        &self,
        monitor: &ManifestMonitor,
        remaining_ms: Option<f64>,
        manager: &mut crate::manager::TerminalManager,
        registry: &mut crate::monitor_registry::MonitorRegistry,
    ) -> crate::durable_rpc::RpcRestoreResult {
        let source = registry.rpc_source();
        let mut arm = |options| {
            let (id, _, start) = source.arm(options)?;
            tokio::spawn(start);
            Ok(id)
        };
        let mut pause = |ids: &[String]| {
            registry.pause(ids);
        };
        let mut bind = |monitor_id: &str, runtime_id: &str| {
            manager.bind_monitor_id(monitor_id, runtime_id);
        };
        crate::durable_rpc::restore_checkpointed_rpc(
            monitor,
            remaining_ms,
            crate::durable_rpc::CheckpointedRpcRestoreDeps {
                arm: &mut arm,
                pause: &mut pause,
                writer: Some(self.rpc_checkpoint_sink()),
                bind_monitor_id: Some(&mut bind),
                session_id: Some(self.session_id.clone()),
                agent_dir: self.rpc_agent_dir.clone(),
            },
        )
    }
    pub async fn observe_monitor_state(
        &mut self,
        snapshot: &[crate::monitor_registry::MonitorSnapshotEntry],
        now: f64,
    ) -> Result<(), String> {
        let mut live = BTreeMap::new();
        for entry in snapshot {
            let id=entry.monitor_id.as_deref().filter(|id|!id.is_empty()).ok_or_else(||format!("terminal manifest writer saw a monitor snapshot entry without a stable monitorId (runtime id {}); surfacing instead of silently skipping it",entry.id))?;
            live.insert(id, entry);
        }
        let mut changed = false;
        self.entries.retain(|id, entry| {
            let Some(current) = live.get(id.as_str()) else {
                if entry.suspended {
                    return true;
                }
                changed = true;
                return false;
            };
            let window = current
                .fire_window
                .as_ref()
                .map(|window| ManifestFireWindow {
                    start_ms: window.start_ms,
                    count: window.count as f64,
                })
                .unwrap_or_else(|| entry.fire_window.clone());
            if entry.delivery_paused != current.paused || entry.fire_window.count != window.count {
                entry.delivery_paused = current.paused;
                entry.fire_window = window;
                changed = true;
            }
            true
        });
        if changed {
            self.persist(now).await;
        }
        Ok(())
    }
    pub fn schedule_checkpoint(&mut self, id: &str, checkpoint: TerminalManifestCheckpoint) {
        self.pending.insert(id.to_owned(), checkpoint);
    }
    /// Upstream `scheduleRpcCheckpoint`: an rpc watch's view (hosts, statuses), absorbed with the
    /// file checkpoints on the next flush or shutdown.
    pub fn schedule_rpc_checkpoint(&mut self, id: &str, checkpoint: TerminalManifestRpcCheckpoint) {
        self.rpc_checkpoint_sink()(id, checkpoint);
    }
    /// `scheduleRpcCheckpoint` as a sink a live rpc watch holds (no writer lock taken).
    pub fn rpc_checkpoint_sink(&self) -> crate::rpc_monitor_source::RpcCheckpointSink {
        let pending = self.pending_rpc.clone();
        std::sync::Arc::new(move |id, checkpoint| {
            pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id.to_owned(), checkpoint);
        })
    }
    fn take_pending_rpc(&self) -> BTreeMap<String, TerminalManifestRpcCheckpoint> {
        std::mem::take(
            &mut self
                .pending_rpc
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
    fn absorb_pending(&mut self) {
        for (id, checkpoint) in std::mem::take(&mut self.pending) {
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.last_checkpoint = Some(checkpoint);
            }
        }
        for (id, checkpoint) in self.take_pending_rpc() {
            if let Some(entry) = self.entries.get_mut(&id) {
                entry.rpc_checkpoint = Some(checkpoint);
            }
        }
    }
    pub async fn flush(&mut self, now: f64) {
        let rpc_pending = !self
            .pending_rpc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty();
        if !self.pending.is_empty() || rpc_pending {
            self.absorb_pending();
            self.persist(now).await;
        }
    }
    pub async fn record_shutdown(&mut self, now: f64) {
        self.absorb_pending();
        for entry in self.entries.values_mut() {
            entry.suspended = true;
        }
        self.persist(now).await;
    }
    pub async fn record_background_start(
        &mut self,
        id: &str,
        command: &str,
        started_at_ms: f64,
        now: f64,
    ) {
        self.backgrounds.insert(
            id.to_owned(),
            ManifestBackgroundSession {
                id: id.to_owned(),
                command: command.to_owned(),
                started_at_ms,
            },
        );
        self.persist(now).await;
    }
    pub async fn record_background_exit(&mut self, id: &str, now: f64) {
        if self.backgrounds.shift_remove(id).is_some() {
            self.persist(now).await;
        }
    }
    async fn persist(&mut self, now: f64) {
        let state = TerminalManifest {
            version: TERMINAL_MANIFEST_VERSION,
            session_id: self.session_id.clone(),
            monitors: self.entries.values().cloned().collect(),
            background_sessions: self.backgrounds.values().cloned().collect(),
            updated_at: now,
        };
        let result = match serde_json::to_value(state) {
            Ok(value) => self.store.write(&value).await,
            Err(error) => Err(SidecarError::Invalid(error.to_string())),
        };
        if let Err(error) = result {
            self.persist_failure = Some(error);
        }
    }
}
pub struct CheckpointDebouncer {
    writer: std::sync::Arc<tokio::sync::Mutex<TerminalManifestWriter>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl CheckpointDebouncer {
    pub fn new(writer: std::sync::Arc<tokio::sync::Mutex<TerminalManifestWriter>>) -> Self {
        Self { writer, task: None }
    }
    pub async fn schedule(&mut self, id: &str, checkpoint: TerminalManifestCheckpoint) {
        self.writer.lock().await.schedule_checkpoint(id, checkpoint);
        if let Some(task) = self.task.take() {
            task.abort();
        }
        let writer = self.writer.clone();
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_millis(TERMINAL_MANIFEST_CHECKPOINT_DEBOUNCE_MS);
        self.task = Some(tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("epoch")
                .as_secs_f64()
                * 1000.0;
            writer.lock().await.flush(now).await;
        }));
    }
    pub async fn flush(&mut self, now: f64) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.writer.lock().await.flush(now).await;
    }
    pub async fn record_shutdown(&mut self, now: f64) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.writer.lock().await.record_shutdown(now).await;
    }
}
impl Drop for CheckpointDebouncer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[tokio::test]
    async fn restore_store_adopts_live_file_and_preserves_mute_deadline_and_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watch");
        std::fs::write(&path, b"same").unwrap();
        let checkpoint = crate::durable_file::file_checkpoint(&path).unwrap();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        writer
            .record_register(
                MonitorRegistration {
                    monitor_id: "mon_saved".to_owned(),
                    spec: MonitorSpec::File {
                        description: "watch".to_owned(),
                        path: "watch".to_owned(),
                        event: FileEvent::Modify,
                        timeout_ms: 300_000.0,
                        cwd: dir.path().to_string_lossy().into_owned(),
                        approved_parent: None,
                        persistent: true,
                    },
                },
                10.0,
            )
            .await;
        writer.entries.get_mut("mon_saved").unwrap().delivery_paused = true;
        writer.schedule_checkpoint("mon_saved", checkpoint.clone());
        writer.record_shutdown(20.0).await;
        let expiry = writer.entries["mon_saved"].expires_at;
        let mut next = TerminalManifestWriter::new(dir.path(), "s");
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let digest = next.restore_live(&mut manager, &mut registry, 30.0).await;
        assert_eq!(
            (digest.muted, digest.restored, digest.lost, digest.expired),
            (1, 0, 0, 0)
        );
        assert_eq!(
            digest
                .results
                .iter()
                .map(|result| (result.monitor_id.as_str(), result.outcome))
                .collect::<Vec<_>>(),
            [("mon_saved", "muted")]
        );
        assert_eq!(next.durable_count(), 1);
        assert!(!next.entries["mon_saved"].suspended);
        assert_eq!(next.entries["mon_saved"].expires_at, expiry);
        assert!(registry.snapshot()[0].paused);
        assert_eq!(manager.resolve_id("mon_saved"), Some("watch_1".to_owned()));
        next.flush(40.0).await;
        let persisted = crate::restore::parse_terminal_manifest(
            &next.store.read().await.unwrap().unwrap(),
            "s",
        )
        .unwrap();
        assert_eq!(persisted.monitors[0].last_checkpoint, Some(checkpoint));
        registry.dispose();
        manager.teardown().unwrap();
    }
    #[tokio::test]
    async fn background_records_follow_insertion_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        writer
            .record_background_start("bash_9", "nine", 1.0, 1.0)
            .await;
        writer
            .record_background_start("bash_2", "two", 2.0, 2.0)
            .await;
        let state = crate::restore::parse_terminal_manifest(
            &writer.store.read().await.unwrap().unwrap(),
            "s",
        )
        .unwrap();
        assert_eq!(
            state
                .background_sessions
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["bash_9", "bash_2"]
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn expired_and_ephemeral_store_entries_never_spawn_or_adopt() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        for (id, persistent) in [("mon_expired", true), ("mon_ephemeral", false)] {
            writer
                .record_register(
                    MonitorRegistration {
                        monitor_id: id.to_owned(),
                        spec: MonitorSpec::Command {
                            description: id.to_owned(),
                            command: "read value".to_owned(),
                            filter: None,
                            human_input: None,
                            wakes_per_hour: None,
                            cwd: Some(dir.path().to_string_lossy().into_owned()),
                            persistent,
                        },
                    },
                    1.0,
                )
                .await;
        }
        writer
            .record_background_start("bash_old", "old", 1.0, 1.0)
            .await;
        let mut next = TerminalManifestWriter::new(dir.path(), "s");
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let digest = next
            .restore_live(
                &mut manager,
                &mut registry,
                1.0 + DURABLE_MONITOR_EXPIRY_MS as f64,
            )
            .await;
        assert_eq!(
            (digest.expired, digest.lost, digest.restored, digest.muted),
            (1, 2, 0, 0)
        );
        assert_eq!(
            digest
                .results
                .iter()
                .filter(|result| result.outcome == "expired")
                .count(),
            1
        );
        assert_eq!(manager.size(), 0);
        assert!(registry.snapshot().is_empty());
        assert_eq!(next.durable_count(), 0);
        assert!(next.entries.is_empty());
        registry.dispose();
    }
    /// restore.ts: an ephemeral command watch with time left on its recorded `deadlineMs` is handed
    /// to the command handler (and adopted); `humanInput` and `deadlineMs` survive the round-trip.
    #[cfg(unix)]
    #[tokio::test]
    async fn ephemeral_command_with_time_left_is_restored_with_its_deadline_and_human_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        writer
            .record_register_with(
                MonitorRegistration {
                    monitor_id: "mon_ephemeral".to_owned(),
                    spec: MonitorSpec::Command {
                        description: "inbox".to_owned(),
                        command: "read value".to_owned(),
                        filter: None,
                        human_input: Some(true),
                        wakes_per_hour: None,
                        cwd: Some(dir.path().to_string_lossy().into_owned()),
                        persistent: false,
                    },
                }
                .with_deadline_ms(Some(100_000.0)),
                1.0,
            )
            .await;
        let saved = writer.store.read().await.unwrap().unwrap();
        assert_eq!(saved["monitors"][0]["humanInput"], true);
        assert_eq!(saved["monitors"][0]["deadlineMs"], 100_000.0);
        let mut next = TerminalManifestWriter::new(dir.path(), "s");
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let digest = next
            .restore_live(&mut manager, &mut registry, 40_000.0)
            .await;
        assert_eq!((digest.restored, digest.lost), (1, 0));
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].monitor_id.as_deref(), Some("mon_ephemeral"));
        assert_eq!(snapshot[0].persistent, Some(false));
        assert!(snapshot[0].deadline_ms.is_some());
        assert_eq!(next.entries["mon_ephemeral"].human_input, Some(true));
        registry.dispose();
        manager.teardown().unwrap();
        // Past its deadline the same entry is lost and never spawns.
        let mut late = TerminalManifestWriter::new(dir.path(), "s");
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let digest = late
            .restore_live(&mut manager, &mut registry, 100_001.0)
            .await;
        assert_eq!((digest.restored, digest.lost), (0, 1));
        assert_eq!(manager.size(), 0);
        registry.dispose();
    }
    #[tokio::test]
    async fn registration_checkpoint_shutdown_and_adoption_preserve_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        writer
            .record_register(
                MonitorRegistration {
                    monitor_id: "mon_1".to_owned(),
                    spec: MonitorSpec::Command {
                        description: "watch".to_owned(),
                        command: "true".to_owned(),
                        filter: None,
                        human_input: None,
                        wakes_per_hour: None,
                        cwd: Some("/tmp".to_owned()),
                        persistent: true,
                    },
                },
                10.0,
            )
            .await;
        assert_eq!(writer.durable_count(), 1);
        assert!(writer.persist_failure.is_none());
        let checkpoint = TerminalManifestCheckpoint {
            dev: 1.0,
            ino: 2.0,
            size: 3.0,
            mtime_ms: 4.0,
            digest: "saved".to_owned(),
            present: true,
        };
        writer.schedule_checkpoint("mon_1", checkpoint.clone());
        writer.record_shutdown(20.0).await;
        let state = crate::restore::parse_terminal_manifest(
            &writer.store.read().await.unwrap().unwrap(),
            "s",
        )
        .unwrap();
        let entry = state.monitors[0].clone();
        assert!(entry.suspended);
        assert_eq!(entry.last_checkpoint, Some(checkpoint));
        let mut next = TerminalManifestWriter::new(dir.path(), "s");
        next.adopt_restored(entry.clone());
        assert!(!next.entries["mon_1"].suspended);
        next.record_background_start("bash_2", "read", 30.0, 30.0)
            .await;
        let state = crate::restore::parse_terminal_manifest(
            &next.store.read().await.unwrap().unwrap(),
            "s",
        )
        .unwrap();
        assert_eq!(state.monitors[0].expires_at, entry.expires_at);
        assert_eq!(state.monitors[0].created_at, 10.0);
        next.observe_monitor_state(&[], 40.0).await.unwrap();
        assert_eq!(next.durable_count(), 0);
    }
    #[tokio::test]
    #[cfg(unix)]
    async fn configured_store_restore_delivers_real_shell_geometry() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        writer
            .record_register(
                MonitorRegistration {
                    monitor_id: "mon_geometry".to_owned(),
                    spec: MonitorSpec::Command {
                        description: "geometry".to_owned(),
                        command:
                            "stty -echo; stty size; printf 'shell:%s\\n' \"${BASH_VERSION:+bash}\""
                                .to_owned(),
                        filter: None,
                        human_input: None,
                        wakes_per_hour: None,
                        cwd: Some(dir.path().to_string_lossy().into_owned()),
                        persistent: true,
                    },
                },
                1.0,
            )
            .await;
        let mut next = TerminalManifestWriter::new(dir.path(), "s");
        let mut manager = crate::manager::TerminalManager::default();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS;
        settings.default_rows = 33.0;
        settings.default_cols = 91.0;
        assert_eq!(
            next.restore_configured_live(
                &mut manager,
                &mut registry,
                2.0,
                Some("/bin/bash"),
                &settings
            )
            .await
            .restored,
            1
        );
        tokio::time::timeout(std::time::Duration::from_secs(5),async {for expected in ["33 91","shell:bash"] {assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Line {line,..}) if line==expected));}assert!(matches!(events.recv().await,Some(crate::monitor_registry::MonitorEvent::Summary {..})));}).await.unwrap();
        registry.dispose();
        manager.teardown().unwrap();
    }
    /// terminal-manifest.ts `scheduleRpcCheckpoint` + `#entryFor` (rpc): a persistent rpc spec is
    /// the `checkpointed-rpc` class with its target/events/exclude, and its view is absorbed on
    /// flush; a restore re-arms it under its `mon_` id (an agent dir with no endpoints joins none).
    #[tokio::test]
    async fn rpc_spec_records_checkpointed_rpc_absorbs_its_checkpoint_and_restores_under_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        writer
            .record_register(
                MonitorRegistration {
                    monitor_id: "mon_RPC".to_owned(),
                    spec: MonitorSpec::Rpc {
                        description: "kids".to_owned(),
                        target: "all".to_owned(),
                        events: Some(crate::rpc_monitor_targets::RpcEvents::All),
                        exclude: Some(vec!["message_update".to_owned()]),
                        filter: Some("idle".to_owned()),
                        persistent: true,
                    },
                },
                10.0,
            )
            .await;
        let entry = &writer.entries["mon_RPC"];
        assert_eq!(entry.runtime_kind, MonitorRuntimeKind::Rpc);
        assert_eq!(
            entry.durability_class,
            MonitorDurabilityClass::CheckpointedRpc
        );
        assert_eq!(entry.target.as_deref(), Some("all"));
        assert_eq!(entry.cwd, None);
        let checkpoint: TerminalManifestRpcCheckpoint = serde_json::from_value(serde_json::json!({
            "hosts":{"/h.sock":{"instanceId":"i","generation":"3"}},"statuses":{"/h.sock#s1":"idle"}}))
        .unwrap();
        writer.rpc_checkpoint_sink()("mon_RPC", checkpoint.clone());
        writer.flush(20.0).await;
        let persisted = crate::restore::parse_terminal_manifest(
            &writer.store.read().await.unwrap().unwrap(),
            "s",
        )
        .unwrap();
        assert_eq!(persisted.monitors[0].rpc_checkpoint, Some(checkpoint));
        assert_eq!(
            persisted.monitors[0].events,
            Some(crate::rpc_monitor_targets::RpcEvents::All)
        );
        writer.record_shutdown(30.0).await;
        let mut next = TerminalManifestWriter::new(dir.path(), "s");
        next.rpc_agent_dir = Some(dir.path().join("agent"));
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let state = next.read_restore_state().await;
        let digest = next.apply_restore_state(
            state,
            &mut manager,
            &mut registry,
            40.0,
            None,
            &crate::settings::TERMINAL_SETTINGS_DEFAULTS,
        );
        assert_eq!((digest.restored, digest.lost), (1, 0));
        assert_eq!(manager.resolve_id("mon_RPC"), Some("rpc_1".to_owned()));
        assert_eq!(next.durable_count(), 1);
        let snapshot = registry.snapshot();
        assert_eq!(snapshot[0].monitor_id.as_deref(), Some("mon_RPC"));
        assert_eq!(snapshot[0].filter.as_deref(), Some("idle"));
        registry.dispose();
    }
    #[tokio::test]
    async fn missing_stable_identity_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = TerminalManifestWriter::new(dir.path(), "s");
        assert!(
            writer
                .observe_monitor_state(
                    &[crate::monitor_registry::MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        ..Default::default()
                    }],
                    0.0
                )
                .await
                .is_err()
        );
    }
}
/// terminal-restore-session.test.ts (a)/(a2): a persistent command watch records its child's
/// identity; after a crash (the old watcher still running) the next restore confirms and stops it
/// before re-spawning once, reports it as an orphan, and records the NEW child, so a second crash
/// stops the respawned watcher in turn.
#[cfg(all(test, target_os = "linux"))]
mod orphan_tests {
    use super::*;

    fn now_ms() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
    }

    /// Spawn a watch as the monitor tool does (stable id in the environment), and record it with
    /// its runtime identity.
    async fn record_live_watch(
        dir: &std::path::Path,
        manager: &mut crate::manager::TerminalManager,
        monitor_id: &str,
    ) -> crate::process_identity::ChildProcessIdentity {
        let command = "while true; do read line; done";
        let options = crate::tools::spawn::command_options(command, Some(dir), None)
            .unwrap()
            .0
            .env(crate::shared::MONITOR_ENV_ID, monitor_id);
        let id = manager.create(command, options).unwrap();
        manager.bind_monitor_id(monitor_id, &id);
        let identity = manager.get(&id).unwrap().identity().unwrap();
        let mut writer = TerminalManifestWriter::new(dir, "orphans");
        writer
            .record_register_with(
                MonitorRegistration {
                    monitor_id: monitor_id.to_owned(),
                    spec: MonitorSpec::Command {
                        description: "ticker".into(),
                        command: command.into(),
                        filter: None,
                        human_input: None,
                        wakes_per_hour: None,
                        cwd: Some(dir.to_string_lossy().into_owned()),
                        persistent: true,
                    },
                }
                .with_deadline_ms(None)
                .with_runtime(Some(identity.clone())),
                now_ms(),
            )
            .await;
        writer.record_shutdown(now_ms()).await;
        identity
    }

    /// The next process's restore: reap, then re-spawn. Returns the orphan reported and the
    /// identity the manifest now records for the watch.
    async fn restart(
        dir: &std::path::Path,
        manager: &mut crate::manager::TerminalManager,
        registry: &mut crate::monitor_registry::MonitorRegistry,
    ) -> (
        Option<crate::restore_digest::Orphan>,
        crate::process_identity::ChildProcessIdentity,
    ) {
        let mut next = TerminalManifestWriter::new(dir, "orphans");
        let digest = next.restore_live(manager, registry, now_ms()).await;
        assert_eq!(digest.restored, 1, "{digest:?}");
        next.persist_restored(now_ms()).await;
        let saved = next.read_restore_state().await.unwrap().unwrap();
        (
            digest.results[0].orphan.clone(),
            saved.monitors[0].runtime.clone().expect("fresh runtime recorded"),
        )
    }

    /// Resolves once the watcher `monitor_id` runs under in `manager` has exited: the crashed
    /// generation's runtime still observes its child, so the reaper's kill is an exit event.
    async fn exited(manager: &mut crate::manager::TerminalManager, monitor_id: &str) {
        let id = manager.resolve_id(monitor_id).expect("bound watch");
        let mut exit = manager.get(&id).expect("live runtime").subscribe_exit();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            exit.wait_for(Option::is_some),
        )
        .await
        .expect("orphan stopped")
        .expect("exit channel open");
    }

    #[tokio::test]
    async fn a_crash_orphan_is_stopped_and_two_crashes_leave_exactly_one_watcher() {
        let dir = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(dir.path()).unwrap();
        // The crashed generation's manager is leaked: its watcher keeps running, as after a crash.
        let crashed = Box::leak(Box::new(crate::manager::TerminalManager::default()));
        let first = record_live_watch(&dir, crashed, "mon_ORPHANTICKER01").await;
        assert!(crate::orphan_reaper::is_process_alive(first.pid));

        // (a) The next process stops the old watcher, re-spawns once, and reports it.
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let (orphan, respawned) = restart(&dir, &mut manager, &mut registry).await;
        assert_eq!(
            orphan,
            Some(crate::restore_digest::Orphan {
                pid: first.pid,
                action: crate::restore_digest::OrphanAction::Killed
            })
        );
        exited(crashed, "mon_ORPHANTICKER01").await;
        assert_ne!(respawned.pid, first.pid);
        assert!(crate::orphan_reaper::is_process_alive(respawned.pid));
        assert_eq!(manager.size(), 1);

        // (a2) A second crash: the restore recorded the NEW process, so it is the one stopped.
        let crashed_again = Box::leak(Box::new(manager));
        std::mem::forget(registry);
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let (orphan, third) = restart(&dir, &mut manager, &mut registry).await;
        assert_eq!(
            orphan,
            Some(crate::restore_digest::Orphan {
                pid: respawned.pid,
                action: crate::restore_digest::OrphanAction::Killed
            })
        );
        exited(crashed_again, "mon_ORPHANTICKER01").await;
        assert!(crate::orphan_reaper::is_process_alive(third.pid));
        registry.dispose();
        manager.teardown().unwrap();
        let _ = (crashed.teardown(), crashed_again.teardown());
    }

    /// A recorded pid that is no longer this watcher (dead) reports no orphan; a reaper that cannot
    /// confirm it reports it unverified and kills nothing.
    #[tokio::test]
    async fn a_dead_or_unverifiable_orphan_is_reported_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(dir.path()).unwrap();
        let crashed = Box::leak(Box::new(crate::manager::TerminalManager::default()));
        record_live_watch(&dir, crashed, "mon_ORPHANUNVERIF1").await;
        // The watcher is gone before the restart: its recorded pid is dead.
        let id = crashed.resolve_id("mon_ORPHANUNVERIF1").unwrap();
        let mut exit = crashed.get(&id).unwrap().subscribe_exit();
        crashed.teardown().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), exit.wait_for(Option::is_some))
            .await
            .expect("watcher stopped")
            .expect("exit channel open");
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let (orphan, _) = restart(&dir, &mut manager, &mut registry).await;
        assert_eq!(orphan, None);
        registry.dispose();
        manager.teardown().unwrap();

        let mut next = TerminalManifestWriter::new(&dir, "orphans");
        next.reap_orphan = Some(std::sync::Arc::new(|runtime, _| {
            Box::pin(async move {
                Ok(crate::orphan_reaper::ReapResult {
                    action: crate::orphan_reaper::ReapAction::None,
                    reason: Some(format!("previous watcher pid {} unverifiable", runtime.pid)),
                })
            })
        }));
        let mut manager = crate::manager::TerminalManager::default();
        let mut registry = crate::monitor_registry::MonitorRegistry::new(|_| {});
        let digest = next.restore_live(&mut manager, &mut registry, now_ms()).await;
        assert_eq!(
            digest.results[0].orphan.as_ref().map(|orphan| orphan.action),
            Some(crate::restore_digest::OrphanAction::Unverified)
        );
        registry.dispose();
        manager.teardown().unwrap();
    }
}

#[cfg(test)]
mod debounce_tests {
    use super::*;
    #[tokio::test]
    async fn drain_cancels_timer_and_persists_last_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let writer = std::sync::Arc::new(tokio::sync::Mutex::new(TerminalManifestWriter::new(
            dir.path(),
            "s",
        )));
        writer
            .lock()
            .await
            .record_register(
                MonitorRegistration {
                    monitor_id: "mon_1".to_owned(),
                    spec: MonitorSpec::Command {
                        description: "watch".to_owned(),
                        command: "true".to_owned(),
                        filter: None,
                        human_input: None,
                        wakes_per_hour: None,
                        cwd: Some("/tmp".to_owned()),
                        persistent: true,
                    },
                },
                1.0,
            )
            .await;
        let mut debounce = CheckpointDebouncer::new(writer.clone());
        for size in [1.0, 2.0] {
            debounce
                .schedule(
                    "mon_1",
                    TerminalManifestCheckpoint {
                        dev: 1.0,
                        ino: 1.0,
                        size,
                        mtime_ms: 1.0,
                        digest: format!("{size}"),
                        present: true,
                    },
                )
                .await;
        }
        debounce.flush(2.0).await;
        assert!(debounce.task.is_none());
        let state = crate::restore::parse_terminal_manifest(
            &writer.lock().await.store.read().await.unwrap().unwrap(),
            "s",
        )
        .unwrap();
        assert_eq!(
            state.monitors[0].last_checkpoint.as_ref().unwrap().size,
            2.0
        );
    }
}
