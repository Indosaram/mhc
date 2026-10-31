//! WHAT the supervisor knows about the host's activity (senpi `host-lifecycle-activity.ts`): attached public clients,
//! the runs its always-on observer connection sees, and the generation's lifetime owner (`SupervisorOwner`). The
//! observer connection (`connectObserver`) is how the idle decision reads turns. On POSIX it connects through the
//! sealed internal hop with a bounded handshake; a child that accepts and never answers fails the open, and the
//! connection is dropped.
#[cfg(unix)]
use crate::host_lifecycle_proxy::BoxedHop;

/// The observer's internal handshake bound (senpi#3816 review), the same 5 s the raw connect had.
pub const INTERNAL_OBSERVER_HANDSHAKE_MS: u64 = 5_000;

/// Opens one observer connection: sealed with `internal_key` when set, else a raw connect bounded by the same 5 s.
/// On failure the socket is dropped (destroyed).
#[cfg(unix)]
pub async fn connect_observer(
    internal_socket: &std::path::Path,
    internal_key: Option<&[u8; 32]>,
) -> std::io::Result<BoxedHop> {
    let bound = std::time::Duration::from_millis(INTERNAL_OBSERVER_HANDSHAKE_MS);
    let connect = tokio::net::UnixStream::connect(internal_socket);
    match internal_key {
        Some(key) => {
            // The handshake bound covers the connect too: one monotonic timer for the whole open.
            let sealed = tokio::time::timeout(bound, async {
                let socket = connect.await?;
                crate::internal_channel::connect_internal_connection(socket, key, None)
                    .await
                    .map_err(|error| std::io::Error::other(error.0))
            })
            .await
            .map_err(|_| std::io::Error::other("internal_auth_timeout"))??;
            Ok(Box::new(sealed))
        }
        None => {
            let socket = tokio::time::timeout(bound, connect).await.map_err(|_| {
                std::io::Error::other(format!(
                    "observer connection to internal host timed out after {INTERNAL_OBSERVER_HANDSHAKE_MS}ms"
                ))
            })??;
            Ok(Box::new(socket))
        }
    }
}

/// What the supervisor knows about the host's activity (senpi `SupervisorActivity`): attached public clients, and the
/// runs its always-on observer connection sees start and settle on the internal socket. Feeds the idle-exit decision.
/// Clock values are milliseconds on the caller's monotonic clock.
pub struct SupervisorActivity {
    pub decider: crate::host_lifecycle::IdleExitDecider,
    runs: crate::host_run_activity::SessionRunActivity,
    observer: crate::observer_link::ObserverLink,
    attached: u64,
    unclassified: u64,
}

impl SupervisorActivity {
    /// Everything `current` reads exists before any client can be accepted.
    pub fn new(idle_exit_ms: f64) -> Self {
        Self {
            decider: crate::host_lifecycle::IdleExitDecider::new(idle_exit_ms),
            runs: Default::default(),
            observer: Default::default(),
            attached: 0,
            unclassified: 0,
        }
    }

    /// The public clients right now: attached (occupancy) and not yet classified.
    pub fn set_clients(&mut self, attached: u64, unclassified: u64) {
        self.attached = attached;
        self.unclassified = unclassified;
    }

    pub fn unclassified_count(&self) -> u64 {
        self.unclassified
    }

    /// senpi `current`: an unhealthy observer counts as busy for one idle window, never longer.
    pub fn current(&self, now: f64) -> crate::host_lifecycle::HostActivity {
        crate::host_lifecycle::HostActivity {
            connections: self.attached,
            active_turns: crate::observer_link::active_turns_for_idle_decision(
                &crate::observer_link::UnknownActivityInput {
                    healthy: self.observer.healthy(),
                    unhealthy_since: self.observer.unhealthy_since(),
                    now,
                    unknown_grace_ms: self.decider.idle_exit_ms,
                    observed_busy: self.runs.busy_sessions() as u64,
                },
            ),
        }
    }

    /// senpi `refresh`: the idle decision now; `on_activity` runs for anything that holds the host (it resets the
    /// owner grace even for activity that starts and ends between ticks).
    pub fn refresh(
        &mut self,
        now: f64,
        on_activity: impl FnOnce(),
    ) -> crate::host_lifecycle::IdleExitDecision {
        let current = self.current(now);
        if current.connections > 0 || current.active_turns > 0 || self.unclassified > 0 {
            on_activity();
        }
        self.decider
            .update(current.connections, current.active_turns, now)
    }

    /// senpi `quiescent`: owner death never turns an unknown observer snapshot into permission to stop a turn.
    pub fn quiescent(&self) -> bool {
        self.observer.healthy()
            && self.runs.busy_sessions() == 0
            && self.attached == 0
            && self.unclassified == 0
    }

    /// senpi `observeHostEvent`: `true` when the line changed a run and the decision must be re-read.
    pub fn observe_host_event(&mut self, line: &str) -> bool {
        self.runs.observe(line)
    }

    /// The observer connection `socket` opened.
    pub fn observer_opened(&mut self, socket: u64) {
        self.observer.opened(socket);
    }

    /// The observer connection `socket` was lost; `true` when a retry should be armed.
    pub fn observer_lost(&mut self, socket: u64, settled: bool, now: f64) -> bool {
        self.observer.lost(socket, settled, now)
    }

    /// senpi `stopObserver`.
    pub fn stop_observer(&mut self) {
        self.observer.stop();
    }
}

/// Continuous quiet time, in addition to attach holds and confirmed owner death (#3044).
pub const OWNER_EXIT_GRACE_MS: f64 = 2_000.;
const OWNER_PROBE_MAX_INTERVAL_MS: f64 = 5_000.;
const OWNER_RECORD_FILE: &str = "owner.json";

/// The OS start identity of a generation's lifetime owner, never a timestamp estimated from uptime (senpi
/// `HostLifetimeOwner`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostLifetimeOwner {
    pub pid: u32,
    pub start_time: String,
}

/// What `owner.json` says: `Unowned` is an explicit `null`; `Unknown` is an unreadable or old generation, never
/// permission to take over (senpi `readHostOwner`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerRecord {
    Unknown,
    Unowned,
    Owned(HostLifetimeOwner),
}

/// senpi `readHostOwner`.
pub fn read_host_owner(generation_dir: &std::path::Path) -> OwnerRecord {
    let Some(record) = crate::host_daemon_state::parse_json(
        std::fs::read_to_string(generation_dir.join(OWNER_RECORD_FILE))
            .ok()
            .as_deref(),
    ) else {
        return OwnerRecord::Unknown;
    };
    match record.get("owner") {
        Some(serde_json::Value::Null) => OwnerRecord::Unowned,
        Some(serde_json::Value::Object(owner)) => {
            let pid = owner
                .get("pid")
                .and_then(serde_json::Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
                .filter(|pid| *pid > 0);
            let start_time = owner
                .get("startTime")
                .and_then(serde_json::Value::as_str)
                .filter(|start| !start.is_empty());
            match (pid, start_time) {
                (Some(pid), Some(start_time)) => OwnerRecord::Owned(HostLifetimeOwner {
                    pid,
                    start_time: start_time.to_owned(),
                }),
                _ => OwnerRecord::Unknown,
            }
        }
        _ => OwnerRecord::Unknown,
    }
}

/// senpi `sameHostOwner`: one pid AND one incarnation; liveness tolerance must not merge two replacement owners.
pub fn same_host_owner(a: &OwnerRecord, b: &OwnerRecord) -> bool {
    let (OwnerRecord::Owned(a), OwnerRecord::Owned(b)) = (a, b) else {
        return false;
    };
    if a.pid != b.pid {
        return false;
    }
    let started = maho_tool_child::group_ownership::process_start_time_ms(&a.start_time);
    started.is_some()
        && started == maho_tool_child::group_ownership::process_start_time_ms(&b.start_time)
}

/// senpi `hostOwnerGone`: the pid is absent, or a different process now holds it. An unreadable identity is never
/// death.
pub fn host_owner_gone(owner: &HostLifetimeOwner) -> bool {
    use maho_tool_child::group_ownership::{process_start_time_ms, same_process_start_ms};
    if !crate::host_reservations::process_is_running(owner.pid) {
        return true;
    }
    let Some(observed) = crate::host_reservations::read_process_start_time(owner.pid) else {
        return false;
    };
    let observed = process_start_time_ms(&observed);
    let recorded = process_start_time_ms(&owner.start_time);
    observed.is_some() && recorded.is_some() && !same_process_start_ms(observed, recorded)
}

/// The owner identity probe's bound (senpi `hostOwnerGone` reads the identity with a 1 s timeout).
pub const OWNER_PROBE_TIMEOUT_MS: u64 = 1_000;

/// `host_owner_gone` off the supervisor's select loop: the probe runs on the blocking pool (on win32 it spawns a
/// process-table query) and an answer slower than `OWNER_PROBE_TIMEOUT_MS` is unknown, never death.
pub async fn host_owner_gone_bounded(owner: HostLifetimeOwner) -> bool {
    let probe = tokio::task::spawn_blocking(move || host_owner_gone(&owner));
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_millis(OWNER_PROBE_TIMEOUT_MS),
            probe
        )
        .await,
        Ok(Ok(true))
    )
}

/// senpi `writeHostOwner`: the one mutable ownership record. An ensure writes it under the endpoint lock (`None` is
/// an explicit unowned generation); supervisors only read it. Written atomically, private to this user.
pub fn write_host_owner(
    generation_dir: &std::path::Path,
    owner: Option<&HostLifetimeOwner>,
) -> std::io::Result<()> {
    let path = generation_dir.join(OWNER_RECORD_FILE);
    let temporary = generation_dir.join(format!(
        "{OWNER_RECORD_FILE}.{}.{}.tmp",
        std::process::id(),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    let record = serde_json::json!({
        "owner": owner.map(|owner| serde_json::json!({"pid": owner.pid, "startTime": owner.start_time})),
    });
    let written = crate::host_daemon_state::write_state_file(&temporary, &record)
        .map_err(|error| error.source)
        .and_then(|()| std::fs::rename(&temporary, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// senpi `callerHostOwner`: this process by its OS start identity; an unreadable identity refuses ownership.
pub fn caller_host_owner() -> std::io::Result<HostLifetimeOwner> {
    let pid = std::process::id();
    crate::host_reservations::read_process_start_time(pid)
        .filter(|start| maho_tool_child::group_ownership::process_start_time_ms(start).is_some())
        .map(|start_time| HostLifetimeOwner { pid, start_time })
        .ok_or_else(|| std::io::Error::other("cannot establish RPC host owner OS start identity"))
}

/// senpi `claimHostOwner`, called inside the ensure lock: a live or unknown different owner is never silently
/// replaced, and an old generation without an owner record does not support owner lifetime.
pub async fn claim_host_owner(generation_dir: &std::path::Path) -> std::io::Result<()> {
    let previous = read_host_owner(generation_dir);
    if previous == OwnerRecord::Unknown {
        return Err(std::io::Error::other(
            "RPC host does not support owner lifetime registration",
        ));
    }
    let next = caller_host_owner()?;
    if same_host_owner(&previous, &OwnerRecord::Owned(next.clone())) {
        return Ok(());
    }
    if let OwnerRecord::Owned(previous) = previous
        && !host_owner_gone_bounded(previous).await
    {
        return Err(std::io::Error::other(
            "RPC host lifetime owner is still alive or its identity is unknown",
        ));
    }
    write_host_owner(generation_dir, Some(&next))
}

/// senpi `OWNER_WATCH_FD` + `keepOwnerPipe`: the caller-lifetime pipe an owning ensure hands its supervisor. The
/// read end is inherited (`--owner-fd <n>`); the write end stays in this process, close-on-exec, never written, and
/// is retained until that supervisor exits - this process's exit (even SIGKILL) closes it in the kernel, which the
/// supervisor reads as owner death.
///
/// SAFETY-DIVERGENCE(senpi stdio slot 3): Rust's `Command` exposes only the three standard slots and this crate
/// denies the `unsafe` a `pre_exec` `dup2` needs, so the read end is duplicated above the presenter's fd 4 and
/// inherited at that number, which `--owner-fd` names. Win32 launches carry no pipe: the supervisor binds the
/// owner by its recorded OS identity instead.
///
/// Accepted spawn window (the same trade-off as `bind_child_watch_pipe`): the inheritable read end exists from
/// `OwnerPipe::new` until `keep` drops it after the spawn, so a process another thread spawns inside that window
/// also inherits it. Only a read end can leak this way - the write end is close-on-exec from creation (on apple,
/// from the `F_SETFD` right after `pipe`) - so owner death still reads as EOF; closing the window would need the
/// `/bin/sh 3<&N N<&-` wrapper on every owner launch. The supervisor adopts and closes-on-exec its copy at once
/// (`adopt_owner_pipe_fd`), so the host child never inherits it.
#[cfg(unix)]
pub struct OwnerPipe {
    read_end: std::os::fd::OwnedFd,
    write_end: std::os::fd::OwnedFd,
}

/// Win32 launches inherit no owner pipe; this type is never constructed there.
#[cfg(not(unix))]
pub struct OwnerPipe {
    never: std::convert::Infallible,
}

#[cfg(not(unix))]
impl OwnerPipe {
    pub fn args(&self) -> [String; 2] {
        let never = self.never;
        match never {}
    }

    pub fn keep(self, _pid: u32) {
        let never = self.never;
        match never {}
    }
}

/// The lowest descriptor the inherited read end may take: clear of stdio, `OWNER_WATCH_FD` and the presenter's 4.
#[cfg(unix)]
const OWNER_PIPE_MIN_FD: i32 = 10;

#[cfg(unix)]
impl OwnerPipe {
    pub fn new() -> std::io::Result<Self> {
        // Close-on-exec from creation where the platform allows it: a concurrent spawn must never inherit the write
        // end, or the owner's exit would not read as EOF.
        #[cfg(not(target_vendor = "apple"))]
        let (read_end, write_end) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        #[cfg(target_vendor = "apple")]
        let (read_end, write_end) = {
            let (read_end, write_end) = rustix::pipe::pipe()?;
            rustix::io::fcntl_setfd(&write_end, rustix::io::FdFlags::CLOEXEC)?;
            (read_end, write_end)
        };
        // Only the duplicate above the presenter's fd 4 is inherited; the original read end closes here.
        let read_end = rustix::io::fcntl_dupfd_cloexec(&read_end, OWNER_PIPE_MIN_FD)?;
        rustix::io::fcntl_setfd(&read_end, rustix::io::FdFlags::empty())?;
        Ok(Self {
            read_end,
            write_end,
        })
    }

    /// The supervisor argv that names the inherited read end.
    pub fn args(&self) -> [String; 2] {
        use std::os::fd::AsRawFd;
        ["--owner-fd".into(), self.read_end.as_raw_fd().to_string()]
    }

    /// senpi `keepOwnerPipe`: after the spawn, this process drops its read end and retains the write end until the
    /// supervisor `pid` is no longer running (pruned on the next keep), never longer than this process.
    pub fn keep(self, pid: u32) {
        static KEPT: std::sync::Mutex<Vec<(u32, std::os::fd::OwnedFd)>> =
            std::sync::Mutex::new(Vec::new());
        let Self {
            read_end,
            write_end,
        } = self;
        drop(read_end);
        let mut kept = KEPT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        kept.retain(|(kept_pid, _)| crate::host_reservations::process_is_running(*kept_pid));
        kept.push((pid, write_end));
    }
}

/// senpi `startSuccessor`'s owner half: the successor generation inherits its predecessor's lifetime owner record,
/// and the owner pipe only when that owner is this very process (`inheritOwnerPipe`).
pub fn inherit_host_owner(
    predecessor_dir: Option<&std::path::Path>,
    successor_dir: &std::path::Path,
) -> std::io::Result<Option<OwnerPipe>> {
    let owner = match predecessor_dir.map(read_host_owner) {
        Some(OwnerRecord::Owned(owner)) => Some(owner),
        _ => None,
    };
    write_host_owner(successor_dir, owner.as_ref())?;
    let inherit_pipe = owner.is_some_and(|owner| {
        owner.pid == std::process::id()
            && caller_host_owner().is_ok_and(|caller| {
                same_host_owner(&OwnerRecord::Owned(owner), &OwnerRecord::Owned(caller))
            })
    });
    #[cfg(unix)]
    {
        inherit_pipe.then(OwnerPipe::new).transpose()
    }
    #[cfg(not(unix))]
    {
        let _ = inherit_pipe;
        Ok(None)
    }
}

/// senpi `SupervisorOwner`: a fresh start inherits the caller's lifetime pipe; an existing supervisor (or a foreign
/// handoff caller) binds the owner by its OS start identity, probed with a backoff. Failed observations are never
/// death. The owner record is re-read on every check (the polling form upstream degrades to when its directory watch
/// cannot arm), so a first owner claimed on an unowned generation is discovered without a watcher.
pub struct SupervisorOwner {
    generation_dir: std::path::PathBuf,
    owner: OwnerRecord,
    pipe: Option<tokio::sync::oneshot::Receiver<bool>>,
    gone: bool,
    next_probe_at: f64,
    probe_interval_ms: f64,
    idle: crate::host_lifecycle::IdleExitDecider,
    stopped: bool,
}

impl SupervisorOwner {
    pub fn new(generation_dir: std::path::PathBuf) -> Self {
        Self {
            generation_dir,
            owner: OwnerRecord::Unknown,
            pipe: None,
            gone: false,
            next_probe_at: 0.,
            probe_interval_ms: 1_000.,
            idle: crate::host_lifecycle::IdleExitDecider::new(OWNER_EXIT_GRACE_MS),
            stopped: false,
        }
    }

    /// senpi `start`: reads the record and, for an owned generation, binds the inherited pipe (already adopted by
    /// [`adopt_owner_pipe_fd`]). Its EOF marks the bound owner gone; a read error leaves it unknown. Inheritance that
    /// is unavailable on this launch path falls back to the recorded OS identity; an unowned generation closes the
    /// pipe here.
    pub fn start(&mut self, pipe: Option<InheritedOwnerPipe>, now: f64) {
        self.reload(now);
        let (Some(pipe), OwnerRecord::Owned(_)) = (pipe, &self.owner) else {
            return;
        };
        self.pipe = watch_owner_pipe(pipe);
    }

    fn reload(&mut self, now: f64) {
        let next = read_host_owner(&self.generation_dir);
        if self.stopped {
            return;
        }
        if !same_host_owner(&self.owner, &next) {
            self.pipe = None;
            self.gone = false;
            self.next_probe_at = 0.;
            self.probe_interval_ms = 1_000.;
            self.activity(now);
        }
        self.owner = next;
    }

    /// senpi `activity`: anything that holds the host resets the owner grace.
    pub fn activity(&mut self, now: f64) {
        self.idle.update(1, 0, now);
    }

    /// Re-reads the record and the pipe; the bound owner when its identity probe is due now (no pipe, not yet gone,
    /// backoff elapsed). `should_exit` probes exactly this owner.
    pub fn due_probe(&mut self, now: f64) -> Option<HostLifetimeOwner> {
        self.reload(now);
        if let Some(pipe) = &mut self.pipe {
            match pipe.try_recv() {
                Ok(true) => self.gone = true,
                // A failed pipe is unknown: the recorded identity takes over.
                Ok(false) | Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.pipe = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }
        }
        match &self.owner {
            OwnerRecord::Owned(bound)
                if self.pipe.is_none() && !self.gone && now >= self.next_probe_at =>
            {
                Some(bound.clone())
            }
            _ => None,
        }
    }

    /// senpi `shouldExit`: the owner is gone (pipe EOF or a probed identity), the host is quiescent, and both stayed
    /// so for `OWNER_EXIT_GRACE_MS`; the record is re-read before authorizing, so a new owner's write wins.
    pub fn should_exit(
        &mut self,
        quiescent: impl Fn() -> bool,
        now: f64,
        owner_gone: impl FnOnce(&HostLifetimeOwner) -> bool,
    ) -> bool {
        let due = self.due_probe(now);
        let owner = self.owner.clone();
        if let Some(bound) = due {
            self.gone = owner_gone(&bound);
            self.next_probe_at = now + self.probe_interval_ms;
            self.probe_interval_ms = (self.probe_interval_ms * 2.).min(OWNER_PROBE_MAX_INTERVAL_MS);
        }
        let quiet = self.gone && matches!(owner, OwnerRecord::Owned(_)) && quiescent();
        if self.idle.update(u64::from(!quiet), 0, now)
            != crate::host_lifecycle::IdleExitDecision::Exit
        {
            return false;
        }
        !self.stopped
            && same_host_owner(&owner, &read_host_owner(&self.generation_dir))
            && quiescent()
    }

    /// senpi `stop`.
    pub fn stop(&mut self) {
        self.stopped = true;
        self.pipe = None;
    }
}

/// Anything that holds the host between two ticks (senpi `onActivity` / the proxy's `onAdmit`).
pub type OnActivity = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Locks the shared activity; a poisoned lock still holds the last consistent snapshot.
pub fn lock_activity(
    activity: &std::sync::Mutex<SupervisorActivity>,
) -> std::sync::MutexGuard<'_, SupervisorActivity> {
    activity
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The supervisor's owner check over its shared activity (senpi `startSupervisorTicker`'s owner half): owner death
/// ends the generation only while `SupervisorActivity::quiescent` holds - a healthy observer, no run, no attached and
/// no unclassified client - and activity reported between ticks (a client admission, a lost observer) resets the
/// owner grace even when it started and ended before the next tick. The activity lock is only taken in synchronous
/// sections.
pub struct OwnerCheck {
    pub owner: SupervisorOwner,
    pub activity: std::sync::Arc<std::sync::Mutex<SupervisorActivity>>,
    held: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl OwnerCheck {
    pub fn new(
        owner: SupervisorOwner,
        activity: std::sync::Arc<std::sync::Mutex<SupervisorActivity>>,
    ) -> Self {
        Self {
            owner,
            activity,
            held: Default::default(),
        }
    }

    /// senpi `onActivity`/`onAdmit`: callable from any task; the next tick resets the owner grace.
    pub fn on_activity(&self) -> OnActivity {
        let held = self.held.clone();
        std::sync::Arc::new(move || held.store(true, std::sync::atomic::Ordering::SeqCst))
    }

    /// Whether anything held the host since the last tick or holds it now (senpi `refresh` -> `onActivity`).
    fn held(&self, now: f64) -> bool {
        let between_ticks = self.held.swap(false, std::sync::atomic::Ordering::SeqCst);
        let activity = lock_activity(&self.activity);
        let current = activity.current(now);
        between_ticks
            || current.connections > 0
            || current.active_turns > 0
            || activity.unclassified_count() > 0
    }

    /// One ticker pass: activity resets the grace, then the owner decides with the activity's quiescence.
    pub fn should_exit(
        &mut self,
        now: f64,
        owner_gone: impl FnOnce(&HostLifetimeOwner) -> bool,
    ) -> bool {
        if self.held(now) {
            self.owner.activity(now);
        }
        let activity = &self.activity;
        self.owner
            .should_exit(|| lock_activity(activity).quiescent(), now, owner_gone)
    }

    /// The supervisor's ticker pass: a due identity probe runs bounded off the select loop
    /// (`host_owner_gone_bounded`) before the synchronous decision consumes its answer. No lock is held across it.
    pub async fn tick(&mut self, now: f64) -> bool {
        let probed = match self.owner.due_probe(now) {
            Some(bound) => Some((host_owner_gone_bounded(bound.clone()).await, bound)),
            None => None,
        };
        // A record rewritten while the probe ran names a different owner: the answer is not about it.
        self.should_exit(now, |owner| {
            probed.is_some_and(|(gone, bound)| gone && *owner == bound)
        })
    }
}

/// The supervisor's adopted owner read end: an owned descriptor on POSIX; win32 inherits no owner pipe.
#[cfg(unix)]
pub type InheritedOwnerPipe = std::os::fd::OwnedFd;
/// The supervisor's adopted owner read end: an owned descriptor on POSIX; win32 inherits no owner pipe.
#[cfg(not(unix))]
pub type InheritedOwnerPipe = std::convert::Infallible;

/// Takes ownership of the inherited `--owner-fd` descriptor when it is an open pipe (FIFO), and marks it close-on-exec
/// so neither the host child nor anything it spawns inherits the owner read end (upstream overwrites the child's fd 3
/// with its own watch pipe; this crate's presenter fd 4 is adopted the same way). Must run before the supervisor
/// spawns anything. Anything else at that number is left untouched and yields `None`: the owner then binds by its
/// recorded OS identity.
///
/// Adoption happens at most once per process; a second call yields `None` instead of a second owner.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn adopt_owner_pipe_fd(fd: i32) -> Option<InheritedOwnerPipe> {
    use std::os::fd::{BorrowedFd, FromRawFd as _, OwnedFd};
    use std::os::unix::fs::FileTypeExt as _;
    static ADOPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if fd < 3 {
        return None;
    }
    // SAFETY: `fcntl(F_GETFD)` takes no pointers and only reports whether `fd` is open (`-1`/EBADF otherwise).
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return None;
    }
    // SAFETY: `fd` was just observed open, and the launch contract (`--owner-fd <fd>`) reserves it for the owner
    // pipe, so nothing else in this process closes it while this borrow lives.
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let kind = std::fs::File::from(borrowed.try_clone_to_owned().ok()?)
        .metadata()
        .ok()?
        .file_type();
    if !kind.is_fifo() {
        return None;
    }
    if ADOPTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    // SAFETY: `fd` is open and is the pipe this process inherited as its owner read end; the one-shot `ADOPTED` latch
    // makes this the descriptor's only owner, so it is closed exactly once, when the `OwnedFd` drops.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    rustix::io::fcntl_setfd(&owned, rustix::io::FdFlags::CLOEXEC).ok()?;
    Some(owned)
}

/// Win32 inherits no owner pipe: the owner binds by its recorded OS identity.
#[cfg(not(unix))]
pub fn adopt_owner_pipe_fd(_fd: i32) -> Option<InheritedOwnerPipe> {
    None
}

/// Reads the adopted owner pipe on its own detached thread: `true` at EOF, `false` on a read error. A thread that
/// cannot start leaves the owner on its recorded identity.
fn watch_owner_pipe(pipe: InheritedOwnerPipe) -> Option<tokio::sync::oneshot::Receiver<bool>> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut file = std::fs::File::from(pipe);
        let (sender, receiver) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("rpc-host-owner".into())
            .spawn(move || {
                let mut buffer = [0u8; 256];
                let eof = loop {
                    match file.read(&mut buffer) {
                        Ok(0) => break true,
                        Ok(_) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(_) => break false,
                    }
                };
                let _ = sender.send(eof);
            })
            .ok()?;
        Some(receiver)
    }
    #[cfg(not(unix))]
    {
        match pipe {}
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;
    use crate::host_lifecycle::IdleExitDecision;

    #[test]
    fn idle_exit_waits_for_quiet_and_an_unhealthy_observer_holds_one_window() {
        let mut activity = SupervisorActivity::new(1_000.);
        activity.observer_opened(1);
        let mut held = 0;
        assert_eq!(activity.refresh(0., || held += 1), IdleExitDecision::Idle);
        assert!(activity.observe_host_event(r#"{"type":"agent_start","sessionId":"s"}"#));
        assert_eq!(
            activity.refresh(500., || held += 1),
            IdleExitDecision::Active
        );
        assert!(!activity.quiescent());
        activity.observe_host_event(r#"{"type":"agent_settled","sessionId":"s"}"#);
        assert!(activity.quiescent());
        assert_eq!(activity.refresh(600., || held += 1), IdleExitDecision::Idle);
        assert_eq!(
            activity.refresh(1_600., || held += 1),
            IdleExitDecision::Exit
        );
        assert_eq!(held, 1);
        // A lost observer is unknown activity: busy for one idle window, never quiescent.
        assert!(activity.observer_lost(1, false, 2_000.));
        assert!(!activity.quiescent());
        assert_eq!(activity.current(2_500.).active_turns, 1);
        assert_eq!(activity.current(3_000.).active_turns, 0);
    }

    #[test]
    fn an_unclassified_client_is_not_occupancy_but_blocks_quiescence() {
        let mut activity = SupervisorActivity::new(1_000.);
        activity.observer_opened(1);
        activity.set_clients(0, 1);
        let mut held = false;
        assert_eq!(activity.refresh(0., || held = true), IdleExitDecision::Idle);
        assert!(held);
        assert_eq!(activity.unclassified_count(), 1);
        assert!(!activity.quiescent());
        activity.set_clients(1, 0);
        assert_eq!(activity.current(0.).connections, 1);
    }

    fn owner_record(dir: &std::path::Path, owner: serde_json::Value) {
        std::fs::write(
            dir.join(OWNER_RECORD_FILE),
            serde_json::json!({ "owner": owner }).to_string(),
        )
        .unwrap();
    }

    fn this_owner() -> serde_json::Value {
        serde_json::json!({
            "pid": std::process::id(),
            "startTime": crate::host_reservations::read_process_start_time(std::process::id()).unwrap(),
        })
    }

    #[test]
    fn the_owner_record_distinguishes_unowned_from_unknown() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(read_host_owner(temp.path()), OwnerRecord::Unknown);
        owner_record(temp.path(), serde_json::Value::Null);
        assert_eq!(read_host_owner(temp.path()), OwnerRecord::Unowned);
        owner_record(temp.path(), serde_json::json!({"pid": 0, "startTime": "1"}));
        assert_eq!(read_host_owner(temp.path()), OwnerRecord::Unknown);
        owner_record(temp.path(), this_owner());
        let owned = read_host_owner(temp.path());
        assert!(matches!(owned, OwnerRecord::Owned(_)));
        assert!(same_host_owner(&owned, &owned));
        assert!(!same_host_owner(&owned, &OwnerRecord::Unowned));
        let OwnerRecord::Owned(owner) = owned else {
            unreachable!()
        };
        assert!(!host_owner_gone(&owner));
    }

    #[test]
    fn a_gone_owner_exits_only_after_the_quiet_grace_and_never_while_busy() {
        let temp = tempfile::tempdir().unwrap();
        owner_record(temp.path(), this_owner());
        let mut owner = SupervisorOwner::new(temp.path().into());
        owner.start(None, 0.);
        let probes = std::cell::Cell::new(0);
        let gone = |_: &HostLifetimeOwner| {
            probes.set(probes.get() + 1);
            true
        };
        assert!(!owner.should_exit(|| true, 0., gone));
        assert!(!owner.should_exit(|| true, 1_999., |_| true));
        assert!(owner.should_exit(|| true, 2_000., |_| true));
        assert_eq!(probes.get(), 1, "a gone owner is not probed again");
        // Busy work resets the grace: owner death never stops a turn.
        assert!(!owner.should_exit(|| false, 2_100., |_| true));
        assert!(!owner.should_exit(|| true, 4_000., |_| true));
        assert!(!owner.should_exit(|| true, 5_999., |_| true));
        assert!(owner.should_exit(|| true, 6_000., |_| true));
        owner.stop();
        assert!(!owner.should_exit(|| true, 10_000., |_| true));
    }

    #[test]
    fn a_live_owner_is_probed_with_backoff_and_a_replaced_record_starts_over() {
        let temp = tempfile::tempdir().unwrap();
        owner_record(temp.path(), this_owner());
        let mut owner = SupervisorOwner::new(temp.path().into());
        owner.start(None, 0.);
        let probes = std::cell::RefCell::new(Vec::new());
        for now in [0., 500., 1_000., 2_000., 2_999., 3_000., 7_000., 12_000.] {
            assert!(!owner.should_exit(
                || true,
                now,
                |_| {
                    probes.borrow_mut().push(now);
                    false
                }
            ));
        }
        assert_eq!(*probes.borrow(), [0., 1_000., 3_000., 7_000., 12_000.]);
        // An unowned generation never exits on owner grounds.
        owner_record(temp.path(), serde_json::Value::Null);
        assert!(!owner.should_exit(|| true, 20_000., |_| true));
        assert!(!owner.should_exit(|| true, 30_000., |_| true));
    }

    fn owner_check(dir: &std::path::Path) -> OwnerCheck {
        owner_record(dir, this_owner());
        let mut owner = SupervisorOwner::new(dir.into());
        owner.start(None, 0.);
        OwnerCheck::new(
            owner,
            std::sync::Arc::new(std::sync::Mutex::new(SupervisorActivity::new(1_000.))),
        )
    }

    #[test]
    fn owner_death_never_ends_a_generation_whose_observer_is_unhealthy_or_never_connected() {
        let temp = tempfile::tempdir().unwrap();
        let mut check = owner_check(temp.path());
        // No observer connection yet: zero turns, zero clients, still never quiescent.
        for now in [0., 2_000., 4_000., 10_000.] {
            assert!(!check.should_exit(now, |_| true));
        }
        lock_activity(&check.activity).observer_opened(1);
        assert!(!check.should_exit(10_500., |_| true));
        assert!(check.should_exit(12_500., |_| true));
        // A lost observer withdraws quiescence again, past its unknown window too.
        let mut check = owner_check(temp.path());
        lock_activity(&check.activity).observer_opened(1);
        assert!(lock_activity(&check.activity).observer_lost(1, false, 0.));
        for now in [0., 2_000., 4_000., 10_000.] {
            assert!(!check.should_exit(now, |_| true));
        }
    }

    #[test]
    fn an_unclassified_client_blocks_owner_exit_and_an_admission_between_ticks_resets_the_grace() {
        let temp = tempfile::tempdir().unwrap();
        let mut check = owner_check(temp.path());
        lock_activity(&check.activity).observer_opened(1);
        lock_activity(&check.activity).set_clients(0, 1);
        assert!(!check.should_exit(0., |_| true));
        assert!(!check.should_exit(5_000., |_| true));
        lock_activity(&check.activity).set_clients(0, 0);
        assert!(!check.should_exit(5_100., |_| true));
        // A client admitted and gone between two ticks still restarts the quiet window.
        (check.on_activity())();
        assert!(!check.should_exit(7_100., |_| true));
        assert!(!check.should_exit(9_099., |_| true));
        assert!(check.should_exit(9_100., |_| true));
    }

    #[tokio::test]
    async fn the_ticker_probes_a_live_owner_off_the_loop_and_never_exits_for_it() {
        let temp = tempfile::tempdir().unwrap();
        let mut check = owner_check(temp.path());
        lock_activity(&check.activity).observer_opened(1);
        let OwnerRecord::Owned(owner) = read_host_owner(temp.path()) else {
            unreachable!()
        };
        assert!(!host_owner_gone_bounded(owner).await);
        // Every probe the backoff allows over 30 s answers "alive": a quiescent host is never ended.
        for now in [0., 1_000., 3_000., 7_000., 12_000., 17_000., 30_000.] {
            assert!(!check.tick(now).await);
        }
        // A record that names nobody is never probed and never authorizes exit.
        owner_record(temp.path(), serde_json::Value::Null);
        assert_eq!(check.owner.due_probe(31_000.), None);
        assert!(!check.tick(40_000.).await);
    }

    #[tokio::test]
    async fn an_ensure_claims_only_an_unowned_or_its_own_or_a_dead_owner_record() {
        let temp = tempfile::tempdir().unwrap();
        // An old generation without a record does not support owner lifetime.
        assert!(claim_host_owner(temp.path()).await.is_err());
        write_host_owner(temp.path(), None).unwrap();
        assert_eq!(read_host_owner(temp.path()), OwnerRecord::Unowned);
        claim_host_owner(temp.path()).await.unwrap();
        let caller = caller_host_owner().unwrap();
        assert_eq!(
            read_host_owner(temp.path()),
            OwnerRecord::Owned(caller.clone())
        );
        // Claiming again as the same owner is a no-op.
        claim_host_owner(temp.path()).await.unwrap();
        // A different, live owner is never replaced.
        #[cfg(unix)]
        {
            let parent = std::os::unix::process::parent_id();
            let live = HostLifetimeOwner {
                pid: parent,
                start_time: crate::host_reservations::read_process_start_time(parent).unwrap(),
            };
            write_host_owner(temp.path(), Some(&live)).unwrap();
            assert!(claim_host_owner(temp.path()).await.is_err());
            assert_eq!(read_host_owner(temp.path()), OwnerRecord::Owned(live));
        }
        // An owner whose pid now holds a different incarnation is gone and may be replaced.
        let replaced = HostLifetimeOwner {
            pid: caller.pid,
            start_time: "1".into(),
        };
        write_host_owner(temp.path(), Some(&replaced)).unwrap();
        claim_host_owner(temp.path()).await.unwrap();
        assert_eq!(read_host_owner(temp.path()), OwnerRecord::Owned(caller));
        // Only the record itself is left behind: the atomic write's temporary is renamed away.
        let names: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [std::ffi::OsString::from(OWNER_RECORD_FILE)]);
    }

    #[test]
    fn a_successor_inherits_the_owner_record_and_this_process_keeps_its_pipe() {
        let temp = tempfile::tempdir().unwrap();
        let (old, new) = (temp.path().join("old"), temp.path().join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        // No predecessor record: the successor is explicitly unowned.
        assert!(inherit_host_owner(Some(&old), &new).unwrap().is_none());
        assert_eq!(read_host_owner(&new), OwnerRecord::Unowned);
        let caller = caller_host_owner().unwrap();
        write_host_owner(&old, Some(&caller)).unwrap();
        let pipe = inherit_host_owner(Some(&old), &new).unwrap();
        assert_eq!(read_host_owner(&new), OwnerRecord::Owned(caller));
        assert_eq!(pipe.is_some(), cfg!(unix));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_owner_pipe_is_named_by_owner_fd_and_its_write_end_close_is_owner_death() {
        let pipe = OwnerPipe::new().unwrap();
        let args = pipe.args();
        let fd: i32 = args[1].parse().unwrap();
        assert!(fd >= OWNER_PIPE_MIN_FD);
        let launch = crate::host_lifecycle_launch::parse_supervisor_args(&[
            "--socket".into(),
            "s".into(),
            args[0].clone(),
            args[1].clone(),
        ])
        .unwrap();
        assert_eq!(launch.owner_fd, Some(fd));
        // The supervisor side reads the inherited descriptor; the ensure's write end closing is EOF.
        let OwnerPipe {
            read_end,
            write_end,
        } = pipe;
        let receiver = watch_owner_pipe(read_end).unwrap();
        drop(write_end);
        let eof = tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
            .await
            .unwrap()
            .unwrap();
        assert!(eof);
    }

    /// The only test that adopts: adoption is one-shot per process.
    #[cfg(unix)]
    #[test]
    fn the_adopted_owner_fd_is_close_on_exec_and_adopted_once() {
        use std::os::fd::{AsRawFd, IntoRawFd};
        let (read_end, write_end) = rustix::pipe::pipe().unwrap();
        // As inherited: open and NOT close-on-exec.
        rustix::io::fcntl_setfd(&read_end, rustix::io::FdFlags::empty()).unwrap();
        let fd = read_end.into_raw_fd();
        // Stdio is never adopted, whatever it is.
        assert!(adopt_owner_pipe_fd(2).is_none());
        let adopted = adopt_owner_pipe_fd(fd).unwrap();
        assert_eq!(adopted.as_raw_fd(), fd);
        assert!(
            rustix::io::fcntl_getfd(&adopted)
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        assert!(adopt_owner_pipe_fd(fd).is_none());
        drop(write_end);
        drop(adopted);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_inherited_owner_pipe_eof_is_owner_death() {
        let temp = tempfile::tempdir().unwrap();
        owner_record(temp.path(), this_owner());
        let (read_end, write_end) = rustix::pipe::pipe().unwrap();
        let mut owner = SupervisorOwner::new(temp.path().into());
        owner.start(Some(read_end), 0.);
        assert!(owner.pipe.is_some());
        drop(write_end);
        let receiver = owner.pipe.take().unwrap();
        let eof = tokio::time::timeout(std::time::Duration::from_secs(5), receiver)
            .await
            .unwrap()
            .unwrap();
        assert!(eof);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn a_child_that_accepts_and_never_answers_fails_the_open_within_the_bound() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("host.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let held = tokio::spawn(async move { listener.accept().await.map(|(socket, _)| socket) });
        let started = tokio::time::Instant::now();
        let error = connect_observer(&path, Some(&[7; 32]))
            .await
            .err()
            .expect("timed out");
        assert_eq!(error.to_string(), "internal_auth_timeout");
        assert_eq!(
            started.elapsed(),
            std::time::Duration::from_millis(INTERNAL_OBSERVER_HANDSHAKE_MS)
        );
        drop(held);
    }
}
