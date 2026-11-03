use crate::runtime_session::{RuntimeError, TerminalRuntimeSession};
use crate::shared::DEFAULT_MAX_SESSIONS;
use maho_pty::PtySessionOptions;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
// registry.ts: DEFAULT_COMMAND / DEFAULT_STOP_EXIT_GRACE_MS / DEFAULT_FORCED_EXIT_GRACE_MS.
const DEFAULT_COMMAND: &str = "bash";
const DEFAULT_STOP_EXIT_GRACE_MS: u64 = 5000;
const DEFAULT_FORCED_EXIT_GRACE_MS: u64 = 1000;
const MAX_STOP_EXIT_GRACE_MS: f64 = 2_147_483_647.0;
/// registry-detached.ts `DEFAULT_DETACHED_EXIT_GRACE_MS`: how long a detached child may take to
/// exit after SIGTERM before it is SIGKILLed.
pub const DEFAULT_DETACHED_EXIT_GRACE_MS: u64 = 1000;
/// registry-types.ts `TerminalSessionState`: a session that outlived even the forced signal stays
/// `Stopping` until its exit is observed; it never reads `Exited` before it really exited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalSessionState {
    Live,
    Stopping,
    Exited,
}
/// The two signals the registry sends (`DEFAULT_SIGNAL` / `FORCE_SIGNAL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetachedSignal {
    Term,
    Kill,
}
impl DetachedSignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Term => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }
}
pub type ExitedProbe = Arc<dyn Fn() -> bool + Send + Sync>;
pub type ChildKill = Arc<dyn Fn(DetachedSignal) + Send + Sync>;
/// Delivers a signal to a pid (or, negated, a process group); an `ESRCH` error means already gone.
pub type KillProcess = Arc<dyn Fn(i32, DetachedSignal) -> std::io::Result<()> + Send + Sync>;
/// registry-types.ts `TrackedDetachedChild`: a process a session started in its own group that
/// must not outlive the session once the registry forgets it.
#[derive(Clone, Default)]
pub struct TrackedDetachedChild {
    pub pid: Option<i32>,
    pub process_group_id: Option<i32>,
    pub exited: Option<ExitedProbe>,
    pub kill: Option<ChildKill>,
}
impl TrackedDetachedChild {
    fn is_exited(&self) -> bool {
        self.exited.as_ref().is_some_and(|exited| exited())
    }
}
/// registry-detached.ts `getDetachedChildKillTarget`: the negated process group off Windows, else
/// the pid; `None` when neither is a positive integer.
pub fn detached_child_kill_target(child: &TrackedDetachedChild, windows: bool) -> Option<i32> {
    match (child.process_group_id, child.pid) {
        (Some(group), _) if !windows && group > 0 => Some(-group),
        (_, Some(pid)) if pid > 0 => Some(pid),
        _ => None,
    }
}
/// registry-detached.ts `defaultKillProcess` (`process.kill(target, signal)`).
pub fn default_kill_process(target: i32, signal: DetachedSignal) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let signal = match signal {
            DetachedSignal::Term => nix::sys::signal::Signal::SIGTERM,
            DetachedSignal::Kill => nix::sys::signal::Signal::SIGKILL,
        };
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(target), signal)
            .map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))
    }
    #[cfg(not(unix))]
    {
        let _ = (target, signal);
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}
fn is_missing_process_error(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(nix::errno::Errno::ESRCH as i32)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}
/// registry-detached.ts `signalDetachedChild`: whether the signal reached a live child. `ESRCH`
/// means the process (or group) is already gone, so there is nothing left to escalate.
fn signal_detached_child(
    child: &TrackedDetachedChild,
    kill_process: &KillProcess,
    signal: DetachedSignal,
) -> Result<bool, RuntimeError> {
    if let Some(kill) = &child.kill {
        kill(signal);
        return Ok(true);
    }
    let Some(target) = detached_child_kill_target(child, cfg!(windows)) else {
        return Ok(false);
    };
    match kill_process(target, signal) {
        Ok(()) => Ok(true),
        Err(error) if is_missing_process_error(&error) => Ok(false),
        Err(error) => Err(RuntimeError::Pty(error.into())),
    }
}
/// A session's tracked detached children and whether they were cleaned up, shared with the task
/// that observes the session's exit. registry.ts memoizes `detachedCleanupPromise`: `done` is that
/// promise, held open by whoever claimed the cleanup until its SIGKILL step finished (the sender is
/// dropped then, so a closed channel reads as settled).
#[derive(Default)]
struct DetachedChildren {
    children: Vec<TrackedDetachedChild>,
    cleaned: bool,
    done: Option<tokio::sync::watch::Receiver<()>>,
}
type SharedDetached = Arc<Mutex<DetachedChildren>>;
/// The cleanups one call claimed (held until its escalation ends) and the in-flight ones another
/// caller claimed first (awaited by the shared paths).
#[derive(Default)]
struct CleanupClaims {
    held: Vec<tokio::sync::watch::Sender<()>>,
    pending: Vec<tokio::sync::watch::Receiver<()>>,
}
impl CleanupClaims {
    /// Upstream `await detachedCleanupPromise` for a cleanup another caller already started.
    async fn settle_pending(&mut self) {
        for mut done in std::mem::take(&mut self.pending) {
            let _ = done.wait_for(|_| false).await;
        }
    }
}
/// registry-detached.ts `cleanupDetachedChildren`, first half: SIGTERM every live tracked child
/// once, and hand back the ones the signal reached.
fn signal_tracked_children(
    detached: &SharedDetached,
    kill_process: &KillProcess,
    signalled: &mut Vec<TrackedDetachedChild>,
    first_error: &mut Option<RuntimeError>,
    claims: &mut CleanupClaims,
) {
    // The children are cloned out and the lock released before any probe or kill callback runs.
    let children = {
        let mut detached = detached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if detached.cleaned {
            claims.pending.extend(detached.done.clone());
            return;
        }
        detached.cleaned = true;
        let (sender, done) = tokio::sync::watch::channel(());
        detached.done = Some(done);
        claims.held.push(sender);
        detached.children.clone()
    };
    for child in &children {
        if child.is_exited() {
            continue;
        }
        match signal_detached_child(child, kill_process, DetachedSignal::Term) {
            Ok(true) => signalled.push(child.clone()),
            Ok(false) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
}
/// Second half off the caller: await the grace, then SIGKILL the survivors; failures are reported.
/// The caller keeps its claims alive until this returns.
async fn escalate_after_grace(
    kill_process: KillProcess,
    signalled: Vec<TrackedDetachedChild>,
    grace: Duration,
) {
    if !grace.is_zero() {
        tokio::time::sleep(grace).await;
    }
    if let Err(error) = TerminalManager::escalate_detached_children(&kill_process, &signalled) {
        maho_ext_api::diagnostic(&format!("terminal detached child cleanup failed: {error}"));
    }
}
/// registry.ts `normalizeGraceMs`: a finite non-negative grace, capped; otherwise the default.
fn normalize_grace_ms(value: Option<f64>, fallback: u64) -> Duration {
    match value {
        Some(value) if value.is_finite() && value >= 0.0 => {
            Duration::from_millis(value.min(MAX_STOP_EXIT_GRACE_MS) as u64)
        }
        _ => Duration::from_millis(fallback),
    }
}
/// registry-types.ts `stopExitGraceMs` / `forcedExitGraceMs` / `detachedExitGraceMs`.
#[derive(Clone, Copy, Debug, Default)]
pub struct StopGraces {
    pub stop_exit_ms: Option<f64>,
    pub forced_exit_ms: Option<f64>,
    pub detached_exit_ms: Option<f64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Graces {
    stop: Duration,
    forced: Duration,
    detached: Duration,
}
impl Graces {
    /// registry.ts constructor: the SIGTERM grace must be positive (zero falls back to the
    /// default), while the forced and detached graces accept zero.
    fn normalize(graces: StopGraces) -> Self {
        Self {
            stop: match graces.stop_exit_ms {
                Some(value) if value.is_finite() && value > 0.0 => {
                    Duration::from_millis(value.min(MAX_STOP_EXIT_GRACE_MS) as u64)
                }
                _ => Duration::from_millis(DEFAULT_STOP_EXIT_GRACE_MS),
            },
            forced: normalize_grace_ms(graces.forced_exit_ms, DEFAULT_FORCED_EXIT_GRACE_MS),
            detached: normalize_grace_ms(graces.detached_exit_ms, DEFAULT_DETACHED_EXIT_GRACE_MS),
        }
    }
}
/// registry-session.ts `sessionIdPrefix`: the shell basename (Windows executable extension stripped),
/// lowercased with non-alphanumeric runs collapsed to `_`, so ids read `bash_N`, `sh_N`, `pwsh_N`, `cmd_N`.
pub fn session_id_prefix(command: &str) -> String {
    let base_name = command
        .split(['\\', '/'])
        .rfind(|part| !part.is_empty())
        .unwrap_or(DEFAULT_COMMAND);
    let lowered = base_name.to_lowercase();
    let without_ext = [".exe", ".com", ".bat", ".cmd", ".ps1"]
        .iter()
        .find_map(|ext| lowered.strip_suffix(ext))
        .unwrap_or(&lowered);
    let mut prefix = String::new();
    for character in without_ext.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            prefix.push(character);
        } else if !prefix.ends_with('_') {
            prefix.push('_');
        }
    }
    let prefix = prefix.trim_matches('_');
    if prefix.is_empty() {
        "session".to_owned()
    } else {
        prefix.to_owned()
    }
}
type ExitWatch = tokio::sync::watch::Receiver<Option<Result<maho_pty::PtyExit, String>>>;
/// Await the settled exit for at most `grace`; `false` means it did not settle in time.
fn wait_for_terminal_session_exit(
    runtime: &TerminalRuntimeSession,
    grace: Duration,
) -> Result<bool, RuntimeError> {
    match runtime.wait(grace) {
        Err(RuntimeError::WaitTimeout) => Ok(false),
        Err(RuntimeError::Pty(ref err))
            if err.to_string().contains("258")
                || err.to_string().contains("os error 6")
                || err.to_string().contains("os error 5")
                || err.to_string().contains("os error 2") =>
        {
            Ok(false)
        }
        Err(RuntimeError::Poisoned) => Err(RuntimeError::Poisoned),
        _ => runtime.exited(),
    }
}
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("Cannot create terminal session: all {0} registry slots are live.")]
    Capacity(usize),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error("idScope must be letters and digits only.")]
    InvalidIdScope,
}
struct Entry {
    runtime: TerminalRuntimeSession,
    created_at: u64,
    last_used_at: u64,
    stop_requested: bool,
    force_killed: bool,
    /// Set once the forced signal's grace expired with the session still running.
    stopping: bool,
    exited_at: Option<f64>,
    detached: SharedDetached,
}
pub struct TerminalManager {
    entries: BTreeMap<String, Entry>,
    monitor_ids: BTreeMap<String, String>,
    max_sessions: usize,
    scrollback: usize,
    redact_secrets_in_output: bool,
    id_counters: BTreeMap<String, u64>,
    id_scope: Option<String>,
    sequence: u64,
    reservations: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    graces: Graces,
    kill_process: KillProcess,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
}
pub struct CapacityReservation(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl Drop for CapacityReservation {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl Default for TerminalManager {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SESSIONS)
    }
}
impl TerminalManager {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            entries: BTreeMap::new(),
            monitor_ids: BTreeMap::new(),
            max_sessions,
            scrollback: crate::shared::DEFAULT_SCROLLBACK,
            redact_secrets_in_output: false,
            id_counters: BTreeMap::new(),
            id_scope: None,
            sequence: 0,
            reservations: Default::default(),
            graces: Graces::normalize(StopGraces::default()),
            kill_process: Arc::new(default_kill_process),
            now: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
            }),
        }
    }
    /// `SessionRegistryOptions.stopExitGraceMs` / `forcedExitGraceMs` / `detachedExitGraceMs`.
    pub fn with_stop_graces(mut self, graces: StopGraces) -> Self {
        self.graces = Graces::normalize(graces);
        self
    }
    /// `SessionRegistryOptions.killProcess`: how a tracked detached child is signalled.
    pub fn with_kill_process(mut self, kill_process: KillProcess) -> Self {
        self.kill_process = kill_process;
        self
    }
    /// `SessionRegistryOptions.now`: the clock `exitedAt` timestamps are read from.
    pub fn with_clock(mut self, now: Arc<dyn Fn() -> f64 + Send + Sync>) -> Self {
        self.now = now;
        self
    }
    /// registry.ts `timestamp`: `now * 1000 + sequence`, strictly increasing within one clock tick.
    fn timestamp(&mut self) -> f64 {
        self.sequence += 1;
        (self.now)() * 1000.0 + self.sequence as f64
    }
    /// Record a detached child the session `id` started (`session.trackedDetachedChildren`).
    pub fn track_detached_child(&mut self, id: &str, child: TrackedDetachedChild) -> bool {
        let Some(entry) = self.entries.get_mut(id) else {
            return false;
        };
        entry
            .detached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .children
            .push(child);
        true
    }
    /// registry.ts `refreshEntryState` + entry `state`/`exitedAt`: an exited session is stamped
    /// once, when its exit is first observed.
    pub fn session_state(
        &mut self,
        id: &str,
    ) -> Result<Option<(TerminalSessionState, Option<f64>)>, RuntimeError> {
        if !self.entries.contains_key(id) {
            return Ok(None);
        }
        self.refresh_entry_state(id)?;
        let entry = &self.entries[id];
        let state = if entry.exited_at.is_some() {
            TerminalSessionState::Exited
        } else if entry.stopping {
            TerminalSessionState::Stopping
        } else {
            TerminalSessionState::Live
        };
        Ok(Some((state, entry.exited_at)))
    }
    fn refresh_entry_state(&mut self, id: &str) -> Result<(), RuntimeError> {
        let Some(entry) = self.entries.get(id) else {
            return Ok(());
        };
        if entry.exited_at.is_some() || !entry.runtime.exited()? {
            return Ok(());
        }
        let timestamp = self.timestamp();
        if let Some(entry) = self.entries.get_mut(id) {
            entry.exited_at = Some(timestamp);
            entry.stopping = false;
        }
        Ok(())
    }
    /// registry.ts `sweepExited`: clean up the detached children of every exited session, and with
    /// `remove` forget those sessions; returns the swept ids.
    pub fn sweep_exited(&mut self, remove: bool) -> Result<Vec<String>, RuntimeError> {
        let ids = self.entries.keys().cloned().collect::<Vec<_>>();
        let mut swept = Vec::new();
        for id in ids {
            self.refresh_entry_state(&id)?;
            if self.entries[&id].exited_at.is_none() {
                continue;
            }
            self.cleanup_detached_children(std::slice::from_ref(&id))?;
            if remove && let Some(entry) = self.entries.remove(&id) {
                entry.runtime.dispose()?;
                self.monitor_ids.retain(|_, runtime_id| runtime_id != &id);
            }
            swept.push(id);
        }
        Ok(swept)
    }
    /// registry-detached.ts `cleanupDetachedChildren`, first half: SIGTERM every live tracked child
    /// of each entry not yet cleaned up, and hand back the ones the signal reached.
    fn signal_detached_children(
        &mut self,
        ids: &[String],
    ) -> (
        Vec<TrackedDetachedChild>,
        Option<RuntimeError>,
        CleanupClaims,
    ) {
        let mut signalled = Vec::new();
        let mut first_error = None;
        let mut claims = CleanupClaims::default();
        for id in ids {
            if let Some(entry) = self.entries.get(id) {
                signal_tracked_children(
                    &entry.detached,
                    &self.kill_process,
                    &mut signalled,
                    &mut first_error,
                    &mut claims,
                );
            }
        }
        (signalled, first_error, claims)
    }
    /// Second half: SIGKILL every signalled child that has not exited after the grace.
    fn escalate_detached_children(
        kill_process: &KillProcess,
        signalled: &[TrackedDetachedChild],
    ) -> Result<(), RuntimeError> {
        let mut first_error = None;
        for child in signalled {
            if child.is_exited() {
                continue;
            }
            if let Err(error) = signal_detached_child(child, kill_process, DetachedSignal::Kill) {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    /// Without the escalation a child that ignores SIGTERM is abandoned the moment the registry
    /// forgets its session. With a nonzero grace on an async runtime the grace-then-SIGKILL step is
    /// spawned, so no runtime thread (nor the caller's manager lock) is held for the grace; only
    /// without a runtime does the synchronous path block for it. This path never waits on a
    /// cleanup already in flight (nor, on a runtime, on its own spawned SIGKILL step): only
    /// `stop_shared` / `teardown_shared` guarantee the SIGKILL step finished before they return.
    fn cleanup_detached_children(&mut self, ids: &[String]) -> Result<(), RuntimeError> {
        let (signalled, first_error, claims) = self.signal_detached_children(ids);
        if signalled.is_empty() {
            return first_error.map_or(Ok(()), Err);
        }
        let grace = self.graces.detached;
        if !grace.is_zero() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let kill_process = self.kill_process.clone();
                handle.spawn(async move {
                    escalate_after_grace(kill_process, signalled, grace).await;
                    drop(claims);
                });
                return first_error.map_or(Ok(()), Err);
            }
            std::thread::sleep(grace);
        }
        let escalated = Self::escalate_detached_children(&self.kill_process, &signalled);
        match (first_error, escalated) {
            (Some(error), _) | (None, Err(error)) => Err(error),
            (None, Ok(())) => Ok(()),
        }
    }
    /// `TerminalManagerOptions.idScope`: session ids become `bash_<scope>_N`, so a tool child's ids never
    /// match the host's. Rejects a scope that is not letters and digits only (registry.ts constructor).
    pub fn with_id_scope(mut self, scope: &str) -> Result<Self, ManagerError> {
        if scope.is_empty()
            || !scope
                .chars()
                .all(|character| character.is_ascii_alphanumeric())
        {
            return Err(ManagerError::InvalidIdScope);
        }
        self.id_scope = Some(scope.to_owned());
        Ok(self)
    }
    pub fn configure(&mut self, settings: &crate::settings::ResolvedTerminalSettings) {
        self.max_sessions = settings.max_sessions as usize;
        self.scrollback = settings.scrollback as usize;
        self.redact_secrets_in_output = settings.redact_secrets_in_output;
    }
    pub fn size(&self) -> usize {
        self.entries.len()
    }
    pub fn active_size(&self) -> Result<usize, RuntimeError> {
        self.entries.values().try_fold(
            self.reservations.load(std::sync::atomic::Ordering::SeqCst),
            |size, entry| Ok(size + usize::from(!entry.runtime.exited()?)),
        )
    }
    pub fn reserve(&mut self) -> Result<Option<CapacityReservation>, RuntimeError> {
        if self.active_size()? >= self.max_sessions {
            return Ok(None);
        }
        self.reservations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some(CapacityReservation(self.reservations.clone())))
    }
    pub fn create(
        &mut self,
        command: &str,
        options: PtySessionOptions,
    ) -> Result<String, ManagerError> {
        let id_command = options.command.clone();
        self.create_with_id_command(command, &id_command, options)
    }
    /// `create` with the command the session id prefix comes from (upstream registers `shell.shell` while the
    /// launch may be wrapped behind a go-line shim): ids read `bash_N` whatever process the PTY starts.
    pub fn create_with_id_command(
        &mut self,
        command: &str,
        id_command: &str,
        options: PtySessionOptions,
    ) -> Result<String, ManagerError> {
        let redact = self.redact_secrets_in_output;
        self.create_with_runtime_options(command, id_command, options, redact)
    }
    /// `create(command, options: TerminalRuntimeOptions)`: `redact_secrets_in_output` is the call's
    /// `redactSecretsInOutput` (spawn.ts passes `ctx.redactSecretsInOutput`).
    pub fn create_with_runtime_options(
        &mut self,
        command: &str,
        id_command: &str,
        options: PtySessionOptions,
        redact_secrets_in_output: bool,
    ) -> Result<String, ManagerError> {
        if self.active_size()? >= self.max_sessions {
            return Err(ManagerError::Capacity(self.max_sessions));
        }
        while self.entries.len() >= self.max_sessions {
            let mut selected = None;
            for (id, entry) in &self.entries {
                if entry.runtime.exited()? {
                    let key = (entry.last_used_at, entry.created_at);
                    if selected.as_ref().is_none_or(|(_, best)| key < *best) {
                        selected = Some((id.clone(), key));
                    }
                }
            }
            let Some((id, _)) = selected else {
                return Err(ManagerError::Capacity(self.max_sessions));
            };
            self.cleanup_detached_children(std::slice::from_ref(&id))?;
            if let Some(entry) = self.entries.remove(&id) {
                entry.runtime.dispose()?;
            }
            self.monitor_ids.retain(|_, runtime_id| runtime_id != &id);
        }
        let prefix = session_id_prefix(id_command);
        let runtime = TerminalRuntimeSession::start_with_runtime_options(
            command,
            options,
            self.scrollback,
            redact_secrets_in_output,
        )?;
        let id = self.allocate_id(&prefix);
        let detached = SharedDetached::default();
        self.watch_session_exit(runtime.subscribe_exit(), detached.clone());
        self.sequence += 1;
        self.entries.insert(
            id.clone(),
            Entry {
                runtime,
                created_at: self.sequence,
                last_used_at: self.sequence,
                stop_requested: false,
                force_killed: false,
                stopping: false,
                exited_at: None,
                detached,
            },
        );
        Ok(id)
    }
    /// registry.ts `handleSessionExit`: once the session's exit is observed its detached children
    /// are cleaned up (once, shared with stop, teardown, prune and sweep). Needs an async runtime to
    /// observe the exit; without one the cleanup waits for those paths.
    fn watch_session_exit(&self, mut exit: ExitWatch, detached: SharedDetached) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (kill_process, grace) = (self.kill_process.clone(), self.graces.detached);
        handle.spawn(async move {
            let observed = exit.wait_for(Option::is_some).await.is_ok();
            if !observed {
                return;
            }
            let (mut signalled, mut first_error) = (Vec::new(), None);
            let mut claims = CleanupClaims::default();
            signal_tracked_children(
                &detached,
                &kill_process,
                &mut signalled,
                &mut first_error,
                &mut claims,
            );
            if let Some(error) = first_error {
                maho_ext_api::diagnostic(&format!(
                    "terminal detached child cleanup failed: {error}"
                ));
            }
            if !signalled.is_empty() {
                escalate_after_grace(kill_process, signalled, grace).await;
            }
            drop(claims);
        });
    }
    /// registry.ts `allocateId`: per-prefix counters, skipping any id still held by an entry.
    fn allocate_id(&mut self, command_prefix: &str) -> String {
        let scoped;
        let prefix = match &self.id_scope {
            Some(scope) => {
                scoped = format!("{command_prefix}_{scope}");
                scoped.as_str()
            }
            None => command_prefix,
        };
        let mut next = self.id_counters.get(prefix).copied().unwrap_or(0) + 1;
        while self.entries.contains_key(&format!("{prefix}_{next}")) {
            next += 1;
        }
        self.id_counters.insert(prefix.to_owned(), next);
        format!("{prefix}_{next}")
    }
    pub fn get(&mut self, id: &str) -> Option<&mut TerminalRuntimeSession> {
        self.sequence += 1;
        let entry = self.entries.get_mut(id)?;
        entry.last_used_at = self.sequence;
        Some(&mut entry.runtime)
    }
    pub fn bind_monitor_id(&mut self, monitor_id: &str, session_id: &str) {
        self.monitor_ids
            .insert(monitor_id.to_owned(), session_id.to_owned());
    }
    /// registry.ts `monitorIdOf`: the stable `mon_` id bound to a runtime session, if any.
    pub fn monitor_id_of(&self, session_id: &str) -> Option<String> {
        self.monitor_ids
            .iter()
            .find(|(_, runtime_id)| runtime_id.as_str() == session_id)
            .map(|(monitor_id, _)| monitor_id.clone())
    }
    pub fn resolve_id(&self, id: &str) -> Option<String> {
        if id.starts_with("mon_") {
            self.monitor_ids.get(id).cloned()
        } else if self.entries.contains_key(id) {
            Some(id.to_owned())
        } else {
            None
        }
    }
    pub fn list(&self) -> Vec<(&str, &TerminalRuntimeSession)> {
        let mut entries = self.entries.iter().collect::<Vec<_>>();
        entries.sort_by_key(|(_, entry)| entry.created_at);
        entries
            .into_iter()
            .map(|(id, entry)| (id.as_str(), &entry.runtime))
            .collect()
    }
    /// Tree-stop one session: SIGTERM, a bounded wait, then SIGKILL (registry.ts `stopEntry`). A
    /// session that outlives the forced signal stays registered as stopping; that is not an error.
    pub fn stop(&mut self, id: &str) -> Result<bool, RuntimeError> {
        if !self.entries.contains_key(id) {
            return Ok(false);
        }
        self.stop_entries(&[id.to_owned()])?;
        Ok(true)
    }
    /// registry.ts `teardown`: stop every session concurrently, SIGKILL every survivor, then forget all
    /// entries even when one is stuck, so later sessions are still stopped and disposed.
    pub fn teardown(&mut self) -> Result<(), RuntimeError> {
        let ids = self.entries.keys().cloned().collect::<Vec<_>>();
        let mut first_error = self.stop_entries(&ids).err();
        if let Err(error) = self.force_kill_entries(&ids) {
            first_error.get_or_insert(error);
        }
        for entry in std::mem::take(&mut self.entries).into_values() {
            if let Err(error) = entry.runtime.dispose() {
                first_error.get_or_insert(error);
            }
        }
        self.monitor_ids.clear();
        self.id_counters.clear();
        first_error.map_or(Ok(()), Err)
    }
    /// SIGTERM every live, not-yet-stopping entry and hand back its exit watch.
    fn signal_stop_entries(
        &mut self,
        ids: &[String],
    ) -> (Vec<(String, ExitWatch)>, Option<RuntimeError>) {
        let mut first_error = None;
        let mut pending = Vec::new();
        for id in ids {
            let Some(entry) = self.entries.get_mut(id) else {
                continue;
            };
            if entry.stop_requested {
                pending.push((id.clone(), entry.runtime.subscribe_exit()));
                continue;
            }
            entry.stop_requested = true;
            match entry.runtime.exited() {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            }
            match entry.runtime.stop() {
                Ok(()) => pending.push((id.clone(), entry.runtime.subscribe_exit())),
                Err(error) => {
                    first_error.get_or_insert(error);
                    if let Err(error) = entry.runtime.kill() {
                        first_error.get_or_insert(error);
                    }
                    entry.runtime.settle_cancelled();
                }
            }
        }
        (pending, first_error)
    }
    /// registry.ts `stopEntry`: the detached children are cleaned up first, then the session is
    /// stopped.
    fn stop_entries(&mut self, ids: &[String]) -> Result<(), RuntimeError> {
        let mut first_error = self.cleanup_detached_children(ids).err();
        let (pending, error) = self.signal_stop_entries(ids);
        if let Some(error) = error {
            first_error.get_or_insert(error);
        }
        match self.wait_entries_exit(
            pending.into_iter().map(|(id, _)| id).collect(),
            self.graces.stop,
        ) {
            Ok(survivors) => {
                if let Err(error) = self.force_kill_entries(&survivors) {
                    first_error.get_or_insert(error);
                }
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    /// registry.ts `forceKillEntry`: SIGKILL once and wait the forced grace; a survivor stays `stopping`.
    fn force_kill_entries(&mut self, ids: &[String]) -> Result<(), RuntimeError> {
        let (pending, mut first_error) = self.signal_force_kill_entries(ids);
        match self.wait_entries_exit(
            pending.into_iter().map(|(id, _)| id).collect(),
            self.graces.forced,
        ) {
            Ok(survivors) => self.mark_stopping(&survivors),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
    /// registry.ts `forceKillEntry` tail: a session that survived the forced signal stays `stopping`.
    fn mark_stopping(&mut self, ids: &[String]) {
        for id in ids {
            if let Some(entry) = self.entries.get_mut(id) {
                entry.stopping = true;
                entry.exited_at = None;
            }
        }
    }
    /// SIGKILL every live entry not yet force-killed and hand back its exit watch.
    fn signal_force_kill_entries(
        &mut self,
        ids: &[String],
    ) -> (Vec<(String, ExitWatch)>, Option<RuntimeError>) {
        let mut first_error = None;
        let mut pending = Vec::new();
        for id in ids {
            let Some(entry) = self.entries.get_mut(id) else {
                continue;
            };
            match entry.runtime.exited() {
                Ok(true) => continue,
                Ok(false) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                    continue;
                }
            }
            if entry.force_killed {
                pending.push((id.clone(), entry.runtime.subscribe_exit()));
                continue;
            }
            entry.force_killed = true;
            match entry.runtime.kill() {
                Ok(()) => pending.push((id.clone(), entry.runtime.subscribe_exit())),
                Err(error) => {
                    first_error.get_or_insert(error);
                    entry.runtime.settle_cancelled();
                }
            }
        }
        (pending, first_error)
    }
    /// Wait for every id against one shared deadline (upstream awaits the stops concurrently).
    fn wait_entries_exit(
        &self,
        ids: Vec<String>,
        grace: Duration,
    ) -> Result<Vec<String>, RuntimeError> {
        let deadline = std::time::Instant::now() + grace;
        let mut survivors = Vec::new();
        for id in ids {
            let Some(entry) = self.entries.get(&id) else {
                continue;
            };
            if !wait_for_terminal_session_exit(
                &entry.runtime,
                deadline.saturating_duration_since(std::time::Instant::now()),
            )? {
                survivors.push(id);
            }
        }
        Ok(survivors)
    }
}
/// Await every exit watch against one shared deadline (upstream races each stop against its grace
/// timer); returns the ids that did not settle in time.
async fn wait_exit_watches(pending: Vec<(String, ExitWatch)>, grace: Duration) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + grace;
    let mut survivors = Vec::new();
    for (id, mut exit) in pending {
        let settled = tokio::time::timeout_at(deadline, exit.wait_for(Option::is_some))
            .await
            .is_ok_and(|result| result.is_ok());
        if !settled {
            survivors.push(id);
        }
    }
    survivors
}
fn lock_manager(
    manager: &Arc<Mutex<TerminalManager>>,
) -> std::sync::MutexGuard<'_, TerminalManager> {
    manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
/// registry-detached.ts `cleanupDetachedChildren` for a shared manager: the grace is awaited with
/// the manager lock released, and a cleanup another caller already started (the session-exit
/// watch, a sync stop) is awaited until its SIGKILL step finished (`await detachedCleanupPromise`).
async fn cleanup_detached_children_shared(
    manager: &Arc<Mutex<TerminalManager>>,
    ids: &[String],
) -> Result<(), RuntimeError> {
    let (signalled, first_error, mut claims, grace, kill_process) = {
        let mut locked = lock_manager(manager);
        let (signalled, error, claims) = locked.signal_detached_children(ids);
        (
            signalled,
            error,
            claims,
            locked.graces.detached,
            locked.kill_process.clone(),
        )
    };
    let escalated = if signalled.is_empty() {
        Ok(())
    } else {
        if !grace.is_zero() {
            tokio::time::sleep(grace).await;
        }
        TerminalManager::escalate_detached_children(&kill_process, &signalled)
    };
    claims.held.clear();
    claims.settle_pending().await;
    match (first_error, escalated) {
        (Some(error), _) | (None, Err(error)) => Err(error),
        (None, Ok(())) => Ok(()),
    }
}
/// SIGTERM wait, then SIGKILL wait for the survivors; the manager lock is only held to signal. A
/// session still running after the forced grace is left `stopping`.
async fn settle_signalled_shared(
    manager: &Arc<Mutex<TerminalManager>>,
    (pending, mut first_error): (Vec<(String, ExitWatch)>, Option<RuntimeError>),
) -> Result<(), RuntimeError> {
    let graces = lock_manager(manager).graces;
    let survivors = wait_exit_watches(pending, graces.stop).await;
    let (pending, error) = lock_manager(manager).signal_force_kill_entries(&survivors);
    if let Some(error) = error {
        first_error.get_or_insert(error);
    }
    let stuck = wait_exit_watches(pending, graces.forced).await;
    lock_manager(manager).mark_stopping(&stuck);
    first_error.map_or(Ok(()), Err)
}
/// registry.ts `stop` for a shared manager: awaits the SIGTERM/SIGKILL graces on the exit watches
/// without holding the manager lock or blocking the async runtime thread.
pub async fn stop_shared(
    manager: &Arc<Mutex<TerminalManager>>,
    id: &str,
) -> Result<bool, RuntimeError> {
    if !lock_manager(manager).entries.contains_key(id) {
        return Ok(false);
    }
    let ids = [id.to_owned()];
    let mut first_error = cleanup_detached_children_shared(manager, &ids).await.err();
    let pending = lock_manager(manager).signal_stop_entries(&ids);
    if let Err(error) = settle_signalled_shared(manager, pending).await {
        first_error.get_or_insert(error);
    }
    first_error.map_or(Ok(true), Err)
}
/// registry.ts `teardown` for a shared manager: stop every session, SIGKILL every survivor, then
/// forget all entries, awaiting each grace off the lock.
pub async fn teardown_shared(manager: &Arc<Mutex<TerminalManager>>) -> Result<(), RuntimeError> {
    let ids = lock_manager(manager)
        .entries
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let mut first_error = cleanup_detached_children_shared(manager, &ids).await.err();
    let pending = lock_manager(manager).signal_stop_entries(&ids);
    if let Err(error) = settle_signalled_shared(manager, pending).await {
        first_error.get_or_insert(error);
    }
    let (pending, error) = lock_manager(manager).signal_force_kill_entries(&ids);
    if let Some(error) = error {
        first_error.get_or_insert(error);
    }
    let forced = lock_manager(manager).graces.forced;
    wait_exit_watches(pending, forced).await;
    let mut manager = manager
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for entry in std::mem::take(&mut manager.entries).into_values() {
        if let Err(error) = entry.runtime.dispose() {
            first_error.get_or_insert(error);
        }
    }
    manager.monitor_ids.clear();
    manager.id_counters.clear();
    first_error.map_or(Ok(()), Err)
}
/// Upstream `isCapacityError`: whether a create failed because every registry slot is live.
pub fn is_capacity_error(error: &ManagerError) -> bool {
    matches!(error, ManagerError::Capacity(_))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn session_teardown_settles_every_subscribed_exit() {
        let manager = Arc::new(Mutex::new(TerminalManager::default()));
        let mut exits = Vec::new();
        for _ in 0..2 {
            let mut state = manager.lock().unwrap();
            let id = state
                .create("read", crate::shared::test_sh_options("read value"))
                .unwrap();
            exits.push(state.get(&id).unwrap().subscribe_exit());
        }
        tokio::time::timeout(Duration::from_secs(10), teardown_shared(&manager))
            .await
            .unwrap()
            .unwrap();
        for mut exit in exits {
            let settled =
                tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some))
                    .await
                    .unwrap()
                    .unwrap()
                    .clone()
                    .unwrap()
                    .unwrap();
            assert!(settled.cancelled);
        }
        assert_eq!(manager.lock().unwrap().size(), 0);
    }
    #[tokio::test]
    async fn configured_capacity_and_zero_scrollback_reach_new_sessions() -> Result<(), ManagerError>
    {
        let mut manager = TerminalManager::default();
        let mut settings = crate::settings::TERMINAL_SETTINGS_DEFAULTS;
        settings.max_sessions = 1.0;
        settings.scrollback = 0.0;
        manager.configure(&settings);
        let id = manager.create(
            "read",
            crate::shared::test_sh_options(
                "stty -echo; read start; printf 'one\\r\\ntwo\\r\\nthree'; read end",
            )
            .size(20, 2),
        )?;
        assert!(manager.reserve()?.is_none());
        let runtime = manager.get(&id).unwrap();
        let (_, mut output) = runtime.subscribe_output()?;
        runtime.write(b"start\n")?;
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut text = String::new();
            while !text.contains("three") {
                text.push_str(&output.recv().await.unwrap());
            }
        })
        .await
        .unwrap();
        assert!(manager.get(&id).unwrap().snapshot()?.scrollback.is_empty());
        manager.teardown()?;
        Ok(())
    }
    #[test]
    fn reservation_counts_until_release() -> Result<(), RuntimeError> {
        let mut manager = TerminalManager::new(1);
        let reservation = manager.reserve()?.expect("capacity");
        assert_eq!(manager.active_size()?, 1);
        assert!(manager.reserve()?.is_none());
        drop(reservation);
        assert_eq!(manager.active_size()?, 0);
        assert!(manager.reserve()?.is_some());
        Ok(())
    }
    #[test]
    fn capacity_stop_and_monitor_bindings() -> Result<(), ManagerError> {
        let mut manager = TerminalManager::new(1);
        let id = manager.create("read", crate::shared::test_sh_options("read value"))?;
        manager.bind_monitor_id("mon_1", &id);
        assert_eq!(manager.resolve_id("mon_1"), Some(id.clone()));
        assert_eq!(manager.active_size()?, 1);
        let refused = manager
            .create("read", crate::shared::test_sh_options("read value"))
            .unwrap_err();
        assert!(matches!(refused, ManagerError::Capacity(1)));
        assert!(is_capacity_error(&refused));
        assert!(!is_capacity_error(&ManagerError::InvalidIdScope));
        assert!(manager.stop(&id)?);
        assert!(manager.get(&id).is_some());
        manager.teardown()?;
        assert_eq!(manager.size(), 0);
        assert_eq!(manager.resolve_id("mon_1"), None);
        Ok(())
    }
    #[test]
    fn id_scope_is_validated_and_scopes_allocated_ids() -> Result<(), ManagerError> {
        for bad in ["", "a_b", "a-b", "a b"] {
            assert!(matches!(
                TerminalManager::default().with_id_scope(bad),
                Err(ManagerError::InvalidIdScope)
            ));
        }
        let mut manager = TerminalManager::default().with_id_scope("Ab12")?;
        assert_eq!(manager.allocate_id("bash"), "bash_Ab12_1");
        assert_eq!(manager.allocate_id("bash"), "bash_Ab12_2");
        assert_eq!(manager.allocate_id("sh"), "sh_Ab12_1");
        assert_eq!(TerminalManager::default().allocate_id("bash"), "bash_1");
        Ok(())
    }
    #[test]
    fn session_ids_use_the_shell_prefix_with_per_prefix_counters() -> Result<(), ManagerError> {
        for (command, prefix) in [
            ("/bin/bash", "bash"),
            ("C:\\Program Files\\Git\\bin\\bash.exe", "bash"),
            ("/bin/sh", "sh"),
            ("pwsh.exe", "pwsh"),
            ("cmd.exe", "cmd"),
            ("/usr/bin/my-Shell", "my_shell"),
            ("/", "bash"),
            ("--", "session"),
        ] {
            assert_eq!(session_id_prefix(command), prefix);
        }
        let mut manager = TerminalManager::new(4);
        let options = crate::shared::test_sh_options("true");
        let prefix = session_id_prefix(&options.command);
        let first = manager.create("a", options.clone())?;
        let second = manager.create("b", options)?;
        assert_eq!(
            (first.as_str(), second.as_str()),
            (
                format!("{prefix}_1").as_str(),
                format!("{prefix}_2").as_str()
            )
        );
        manager.teardown()?;
        Ok(())
    }
    #[cfg(unix)]
    type Killed = Arc<Mutex<Vec<String>>>;
    #[cfg(unix)]
    /// A `killProcess` that records `target:SIGNAL` instead of signalling anything.
    fn recording_kill(killed: &Killed, missing: &'static [i32]) -> KillProcess {
        let killed = killed.clone();
        Arc::new(move |target: i32, signal: DetachedSignal| {
            killed
                .lock()
                .unwrap()
                .push(format!("{target}:{}", signal.as_str()));
            if missing.contains(&target) {
                return Err(std::io::Error::from_raw_os_error(
                    nix::errno::Errno::ESRCH as i32,
                ));
            }
            Ok(())
        })
    }
    #[cfg(unix)]
    fn immediate_graces() -> StopGraces {
        StopGraces {
            detached_exit_ms: Some(0.0),
            ..StopGraces::default()
        }
    }
    #[cfg(unix)]
    async fn exited(manager: &mut TerminalManager, id: &str) {
        let mut exit = manager.get(id).unwrap().subscribe_exit();
        tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap();
    }
    /// registry.test.ts "stops all sessions and tracked detached children idempotently".
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_signals_tracked_detached_children_once_and_escalates() -> Result<(), ManagerError>
    {
        let killed: Killed = Arc::default();
        let mut manager = TerminalManager::new(2)
            .with_stop_graces(immediate_graces())
            .with_kill_process(recording_kill(&killed, &[]));
        let first = manager.create("read", crate::shared::test_sh_options("read value"))?;
        let second = manager.create("read", crate::shared::test_sh_options("read value"))?;
        for id in [&first, &second] {
            assert!(manager.track_detached_child(
                id,
                TrackedDetachedChild {
                    pid: Some(101),
                    process_group_id: Some(201),
                    ..TrackedDetachedChild::default()
                },
            ));
        }
        assert!(!manager.track_detached_child("bash_404", TrackedDetachedChild::default()));
        for _ in 0..2 {
            assert!(manager.stop(&first)?);
            assert!(manager.stop(&second)?);
        }
        let mut signals = killed.lock().unwrap().clone();
        signals.sort();
        assert_eq!(
            signals,
            [
                "-201:SIGKILL",
                "-201:SIGKILL",
                "-201:SIGTERM",
                "-201:SIGTERM"
            ]
        );
        for id in [&first, &second] {
            let (state, exited_at) = manager.session_state(id)?.unwrap();
            assert_eq!(state, TerminalSessionState::Exited);
            assert!(exited_at.is_some());
        }
        manager.teardown()?;
        assert_eq!(killed.lock().unwrap().len(), 4);
        Ok(())
    }
    /// registry-detached.ts: an exited child is skipped, a child gone with ESRCH is not escalated,
    /// a child exposing its own kill is signalled through it, and a child with no target is skipped.
    #[cfg(unix)]
    #[tokio::test]
    async fn detached_cleanup_skips_gone_children_and_prefers_their_own_kill()
    -> Result<(), ManagerError> {
        let killed: Killed = Arc::default();
        let own: Killed = Arc::default();
        let mut manager = TerminalManager::new(1)
            .with_stop_graces(immediate_graces())
            .with_kill_process(recording_kill(&killed, &[302]));
        let id = manager.create("read", crate::shared::test_sh_options("read value"))?;
        let own_kill = own.clone();
        for child in [
            TrackedDetachedChild {
                pid: Some(301),
                exited: Some(Arc::new(|| true)),
                ..TrackedDetachedChild::default()
            },
            TrackedDetachedChild {
                pid: Some(302),
                ..TrackedDetachedChild::default()
            },
            TrackedDetachedChild {
                pid: Some(303),
                kill: Some(Arc::new(move |signal: DetachedSignal| {
                    own_kill.lock().unwrap().push(signal.as_str().to_owned())
                })),
                ..TrackedDetachedChild::default()
            },
            TrackedDetachedChild {
                pid: Some(0),
                process_group_id: Some(-4),
                ..TrackedDetachedChild::default()
            },
        ] {
            manager.track_detached_child(&id, child);
        }
        assert!(manager.stop(&id)?);
        assert_eq!(*killed.lock().unwrap(), ["302:SIGTERM"]);
        assert_eq!(*own.lock().unwrap(), ["SIGTERM", "SIGKILL"]);
        manager.teardown()?;
        Ok(())
    }
    /// registry.test.ts "sweeps exited startup sessions and session-end detached children": an
    /// exited session's children are cleaned up on sweep; `remove` forgets the session and its
    /// monitor binding, while a live session is kept.
    #[cfg(unix)]
    #[tokio::test]
    async fn sweep_exited_cleans_up_children_and_removes_only_exited_sessions()
    -> Result<(), ManagerError> {
        let killed: Killed = Arc::default();
        let mut manager = TerminalManager::new(3)
            .with_stop_graces(immediate_graces())
            .with_kill_process(recording_kill(&killed, &[]));
        let ended = manager.create("true", crate::shared::test_sh_options("true"))?;
        let live = manager.create("read", crate::shared::test_sh_options("read value"))?;
        manager.track_detached_child(
            &ended,
            TrackedDetachedChild {
                pid: Some(301),
                ..TrackedDetachedChild::default()
            },
        );
        manager.track_detached_child(
            &live,
            TrackedDetachedChild {
                pid: Some(401),
                ..TrackedDetachedChild::default()
            },
        );
        manager.bind_monitor_id("mon_ENDED", &ended);
        exited(&mut manager, &ended).await;
        assert_eq!(manager.sweep_exited(false)?.as_slice(), std::slice::from_ref(&ended));
        assert_eq!(
            manager.session_state(&ended)?.unwrap().0,
            TerminalSessionState::Exited
        );
        assert_eq!(*killed.lock().unwrap(), ["301:SIGTERM", "301:SIGKILL"]);
        assert_eq!(manager.sweep_exited(true)?.as_slice(), std::slice::from_ref(&ended));
        assert_eq!(*killed.lock().unwrap(), ["301:SIGTERM", "301:SIGKILL"]);
        assert!(manager.get(&ended).is_none());
        assert_eq!(manager.resolve_id("mon_ENDED"), None);
        assert_eq!(
            manager.session_state(&live)?.unwrap().0,
            TerminalSessionState::Live
        );
        manager.teardown()?;
        assert_eq!(
            *killed.lock().unwrap(),
            ["301:SIGTERM", "301:SIGKILL", "401:SIGTERM", "401:SIGKILL"]
        );
        Ok(())
    }
    /// The LRU prune that makes room for a new session cleans up the evicted session's children.
    #[cfg(unix)]
    #[tokio::test]
    async fn capacity_prune_cleans_up_the_evicted_sessions_children() -> Result<(), ManagerError> {
        let killed: Killed = Arc::default();
        let mut manager = TerminalManager::new(1)
            .with_stop_graces(immediate_graces())
            .with_kill_process(recording_kill(&killed, &[]));
        let ended = manager.create("true", crate::shared::test_sh_options("true"))?;
        manager.track_detached_child(
            &ended,
            TrackedDetachedChild {
                pid: Some(501),
                ..TrackedDetachedChild::default()
            },
        );
        exited(&mut manager, &ended).await;
        manager.create("read", crate::shared::test_sh_options("read value"))?;
        assert!(manager.get(&ended).is_none());
        assert_eq!(*killed.lock().unwrap(), ["501:SIGTERM", "501:SIGKILL"]);
        manager.teardown()?;
        Ok(())
    }
    /// registry-kill-escalation.test.ts "SIGKILLs process groups still alive after the grace and
    /// spares the ones that exited": 402 honours SIGTERM during the grace, so only 401 is killed.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn escalation_spares_children_that_exited_during_the_grace() {
        let killed: Killed = Arc::default();
        let quitter = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let kill_process: KillProcess = {
            let (killed, quitter) = (killed.clone(), quitter.clone());
            Arc::new(move |target: i32, signal: DetachedSignal| {
                killed
                    .lock()
                    .unwrap()
                    .push(format!("{target}:{}", signal.as_str()));
                if target == -402 && signal == DetachedSignal::Term {
                    quitter.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(())
            })
        };
        let detached = SharedDetached::default();
        detached.lock().unwrap().children = vec![
            TrackedDetachedChild {
                pid: Some(401),
                process_group_id: Some(401),
                exited: Some(Arc::new(|| false)),
                ..TrackedDetachedChild::default()
            },
            TrackedDetachedChild {
                pid: Some(402),
                process_group_id: Some(402),
                exited: Some(Arc::new(move || {
                    quitter.load(std::sync::atomic::Ordering::SeqCst)
                })),
                ..TrackedDetachedChild::default()
            },
        ];
        let (mut signalled, mut first_error) = (Vec::new(), None);
        signal_tracked_children(
            &detached,
            &kill_process,
            &mut signalled,
            &mut first_error,
            &mut CleanupClaims::default(),
        );
        assert!(first_error.is_none());
        escalate_after_grace(kill_process, signalled, Duration::from_millis(1000)).await;
        assert_eq!(
            *killed.lock().unwrap(),
            ["-401:SIGTERM", "-402:SIGTERM", "-401:SIGKILL"]
        );
    }
    /// The synchronous cleanup never sleeps on an async runtime: stop returns after SIGTERM and
    /// the grace-then-SIGKILL step runs on its own task.
    #[cfg(unix)]
    #[tokio::test]
    async fn sync_stop_on_a_runtime_does_not_block_for_the_detached_grace()
    -> Result<(), ManagerError> {
        let killed: Killed = Arc::default();
        let mut manager = TerminalManager::new(1)
            .with_stop_graces(StopGraces {
                detached_exit_ms: Some(600_000.0),
                ..StopGraces::default()
            })
            .with_kill_process(recording_kill(&killed, &[]));
        let id = manager.create("read", crate::shared::test_sh_options("read value"))?;
        manager.track_detached_child(
            &id,
            TrackedDetachedChild {
                process_group_id: Some(701),
                ..TrackedDetachedChild::default()
            },
        );
        // A regression that sleeps for the grace fails here instead of hanging for 600 s: the stop
        // runs on a detached thread (runtime entered) and its result is awaited with a bound.
        let (sender, stopped) = tokio::sync::oneshot::channel();
        let (handle, stop_id) = (tokio::runtime::Handle::current(), id.clone());
        std::thread::spawn(move || {
            let _runtime = handle.enter();
            let result = manager.stop(&stop_id);
            let _ = sender.send((result, manager));
        });
        let (result, mut manager) = tokio::time::timeout(Duration::from_secs(30), stopped)
            .await
            .expect("stop returned before the detached grace")
            .unwrap();
        assert!(result?);
        assert_eq!(*killed.lock().unwrap(), ["-701:SIGTERM"]);
        manager.teardown()?;
        assert_eq!(*killed.lock().unwrap(), ["-701:SIGTERM"]);
        Ok(())
    }
    /// registry.ts `handleSessionExit`: a session that exits on its own gets its detached children
    /// cleaned up without any stop, sweep or prune, and only once.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_session_exit_cleans_up_its_detached_children() -> Result<(), ManagerError> {
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut manager = TerminalManager::new(1)
            .with_stop_graces(immediate_graces())
            .with_kill_process(Arc::new(move |target: i32, signal: DetachedSignal| {
                let _ = sender.send(format!("{target}:{}", signal.as_str()));
                Ok(())
            }));
        let id = manager.create("read", crate::shared::test_sh_options("read value"))?;
        manager.track_detached_child(
            &id,
            TrackedDetachedChild {
                process_group_id: Some(801),
                ..TrackedDetachedChild::default()
            },
        );
        manager.get(&id).unwrap().write(b"done\n")?;
        let mut signals = Vec::new();
        for _ in 0..2 {
            let signal = tokio::time::timeout(Duration::from_secs(5), received.recv())
                .await
                .unwrap()
                .unwrap();
            signals.push(signal);
        }
        assert_eq!(signals, ["-801:SIGTERM", "-801:SIGKILL"]);
        assert_eq!(manager.sweep_exited(true)?, [id]);
        assert!(received.try_recv().is_err());
        Ok(())
    }
    /// registry.ts memoized `detachedCleanupPromise`: when the session-exit watch claimed the
    /// cleanup first, `teardown_shared` awaits that cleanup's SIGKILL step instead of returning
    /// mid-grace. Time is paused once the SIGTERM is seen, so the grace only elapses while
    /// teardown waits on it.
    #[cfg(unix)]
    #[tokio::test]
    async fn teardown_shared_awaits_a_cleanup_the_exit_watch_started() {
        let killed: Killed = Arc::default();
        let (sender, mut received) = tokio::sync::mpsc::unbounded_channel::<()>();
        let kill_process: KillProcess = {
            let killed = killed.clone();
            Arc::new(move |target: i32, signal: DetachedSignal| {
                killed
                    .lock()
                    .unwrap()
                    .push(format!("{target}:{}", signal.as_str()));
                let _ = sender.send(());
                Ok(())
            })
        };
        let manager = Arc::new(Mutex::new(
            TerminalManager::new(1)
                .with_stop_graces(StopGraces {
                    detached_exit_ms: Some(1000.0),
                    ..StopGraces::default()
                })
                .with_kill_process(kill_process),
        ));
        {
            let mut locked = manager.lock().unwrap();
            let id = locked
                .create("read", crate::shared::test_sh_options("read value"))
                .unwrap();
            locked.track_detached_child(
                &id,
                TrackedDetachedChild {
                    process_group_id: Some(901),
                    exited: Some(Arc::new(|| false)),
                    ..TrackedDetachedChild::default()
                },
            );
            locked.get(&id).unwrap().write(b"done\n").unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), received.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*killed.lock().unwrap(), ["-901:SIGTERM"]);
        tokio::time::pause();
        teardown_shared(&manager).await.unwrap();
        assert_eq!(*killed.lock().unwrap(), ["-901:SIGTERM", "-901:SIGKILL"]);
    }
    /// The shared (async) stop and teardown run the same once-only detached cleanup.
    #[cfg(unix)]
    #[tokio::test]
    async fn shared_stop_and_teardown_clean_up_detached_children_once() {
        let killed: Killed = Arc::default();
        let manager = Arc::new(Mutex::new(
            TerminalManager::new(1)
                .with_stop_graces(immediate_graces())
                .with_kill_process(recording_kill(&killed, &[])),
        ));
        let id = {
            let mut locked = manager.lock().unwrap();
            let id = locked
                .create("read", crate::shared::test_sh_options("read value"))
                .unwrap();
            locked.track_detached_child(
                &id,
                TrackedDetachedChild {
                    process_group_id: Some(601),
                    ..TrackedDetachedChild::default()
                },
            );
            id
        };
        assert!(stop_shared(&manager, &id).await.unwrap());
        assert!(!stop_shared(&manager, "bash_404").await.unwrap());
        teardown_shared(&manager).await.unwrap();
        assert_eq!(*killed.lock().unwrap(), ["-601:SIGTERM", "-601:SIGKILL"]);
    }
    /// registry.ts `timestamp`: `exitedAt` is read from the injected clock once, when the exit is
    /// first observed.
    #[cfg(unix)]
    #[tokio::test]
    async fn exited_at_is_stamped_once_from_the_injected_clock() -> Result<(), ManagerError> {
        let clock = Arc::new(Mutex::new(7.0));
        let read = clock.clone();
        let mut manager =
            TerminalManager::new(1).with_clock(Arc::new(move || *read.lock().unwrap()));
        let id = manager.create("true", crate::shared::test_sh_options("true"))?;
        exited(&mut manager, &id).await;
        let first = manager.session_state(&id)?.unwrap();
        *clock.lock().unwrap() = 9.0;
        assert_eq!(manager.session_state(&id)?.unwrap(), first);
        assert_eq!(first.0, TerminalSessionState::Exited);
        let stamped = first.1.unwrap();
        assert!(stamped > 7_000.0 && stamped < 8_000.0, "{stamped}");
        assert_eq!(manager.session_state("bash_404")?, None);
        manager.teardown()?;
        Ok(())
    }
    #[test]
    fn kill_target_prefers_the_process_group_off_windows() {
        let child = |pid, process_group_id| TrackedDetachedChild {
            pid,
            process_group_id,
            ..TrackedDetachedChild::default()
        };
        assert_eq!(
            detached_child_kill_target(&child(Some(101), Some(201)), false),
            Some(-201)
        );
        assert_eq!(
            detached_child_kill_target(&child(Some(101), Some(201)), true),
            Some(101)
        );
        assert_eq!(
            detached_child_kill_target(&child(Some(101), Some(0)), false),
            Some(101)
        );
        assert_eq!(
            detached_child_kill_target(&child(Some(-1), None), false),
            None
        );
        assert_eq!(detached_child_kill_target(&child(None, None), false), None);
    }
    #[test]
    fn stop_graces_normalize_like_the_registry_options() {
        let defaults = Graces::normalize(StopGraces::default());
        assert_eq!(
            defaults,
            Graces {
                stop: Duration::from_millis(5000),
                forced: Duration::from_millis(1000),
                detached: Duration::from_millis(1000),
            }
        );
        let zero = Graces::normalize(StopGraces {
            stop_exit_ms: Some(0.0),
            forced_exit_ms: Some(0.0),
            detached_exit_ms: Some(0.0),
        });
        assert_eq!(zero.stop, Duration::from_millis(5000));
        assert_eq!(
            (zero.forced, zero.detached),
            (Duration::ZERO, Duration::ZERO)
        );
        let invalid = Graces::normalize(StopGraces {
            stop_exit_ms: Some(f64::NAN),
            forced_exit_ms: Some(-1.0),
            detached_exit_ms: Some(f64::INFINITY),
        });
        assert_eq!(invalid, defaults);
        let capped = Graces::normalize(StopGraces {
            stop_exit_ms: Some(1e12),
            forced_exit_ms: Some(1e12),
            detached_exit_ms: Some(250.9),
        });
        assert_eq!(capped.stop, Duration::from_millis(2_147_483_647));
        assert_eq!(capped.forced, Duration::from_millis(2_147_483_647));
        assert_eq!(capped.detached, Duration::from_millis(250));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn stop_sends_sigterm_so_traps_clean_up_before_any_sigkill() -> Result<(), ManagerError> {
        let mut manager = TerminalManager::new(1);
        let id=manager.create("trap",PtySessionOptions::new("/bin/sh").arg("-c").arg("stty -echo; trap 'printf cleaned; exit 7' TERM; printf 'ready\\n'; sleep 30 & wait"))?;
        let (history, mut output) = manager.get(&id).expect("session").subscribe_output()?;
        let mut observed = history;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !observed.contains("ready") {
                observed.push_str(&output.recv().await.expect("output"));
            }
        })
        .await
        .expect("ready line");
        assert!(manager.stop(&id)?);
        let runtime = manager.get(&id).expect("session");
        let exit = runtime.exit_result()?.expect("settled exit");
        assert_eq!(exit.exit_code, Some(7));
        assert!(exit.cancelled);
        assert!(runtime.full_output()?.contains("cleaned"));
        manager.teardown()?;
        Ok(())
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn teardown_continues_past_a_session_whose_pty_outlives_sigkill()
    -> Result<(), ManagerError> {
        let mut manager = TerminalManager::new(2);
        let stuck = manager.create(
            "stuck",
            PtySessionOptions::new("/bin/sh")
                .arg("-c")
                .arg("stty -echo; setsid sh -c \"printf 'ready\\\\n'; exec sleep 20\" & wait"),
        )?;
        let other = manager.create(
            "other",
            PtySessionOptions::new("/bin/sh")
                .arg("-c")
                .arg("read value"),
        )?;
        let (history, mut output) = manager.get(&stuck).expect("session").subscribe_output()?;
        let mut observed = history;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !observed.contains("ready") {
                observed.push_str(&output.recv().await.expect("output"));
            }
        })
        .await
        .expect("ready line");
        let mut other_exit = manager.get(&other).expect("session").subscribe_exit();
        assert!(manager.stop(&stuck)?);
        assert!(
            !manager
                .get(&stuck)
                .expect("stopping entry stays registered")
                .exited()?
        );
        // registry.test.ts "keeps a stopped-but-live session in stopping state until exit is observed".
        assert_eq!(
            manager.session_state(&stuck)?,
            Some((TerminalSessionState::Stopping, None))
        );
        manager.teardown()?;
        assert_eq!(manager.size(), 0);
        tokio::time::timeout(Duration::from_secs(5), async {
            while other_exit.borrow_and_update().is_none() {
                other_exit.changed().await.expect("exit sender");
            }
        })
        .await
        .expect("later session stopped");
        Ok(())
    }
}
