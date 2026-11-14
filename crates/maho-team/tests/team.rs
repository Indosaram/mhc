//! Team runs and panes against fake and real children (spec docs/cleanroom/specs/team.md §8).
use maho_lsp::hooks::HookChain;
use maho_task::runner::BoxFuture;
use maho_task::{
    CancelToken, ModelChoice, Notifier, RunOutcome, RunSpec, SpawnRequest, StateDir, TaskManager,
    TaskManagerOptions, TaskObserver, TaskRecord, TaskRunner, TaskSettings, TaskStatus,
    bundled_agents,
};
use maho_team::{MemberSpec, Team, TeamLimits, TeamStatus};
#[cfg(unix)]
use maho_team::{PaneMode, PaneObserver, PaneSettings, Placement, TmuxBackend};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Writes `echo:<prompt>`; a prompt containing `slow` takes 700 ms, `fail` exits 3, `block`
/// waits for cancellation.
#[derive(Default)]
struct FakeRunner {
    running: AtomicUsize,
    peak: AtomicUsize,
    prompts: Mutex<Vec<String>>,
}

impl TaskRunner for FakeRunner {
    fn run<'a>(
        &'a self,
        spec: &'a RunSpec,
        cancel: CancelToken,
        on_start: Box<dyn FnOnce(Option<u32>) + Send + 'a>,
    ) -> BoxFuture<'a, RunOutcome> {
        Box::pin(async move {
            on_start(None);
            lock(&self.prompts).push(spec.prompt.clone());
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            let delay = if spec.prompt.contains("slow") {
                700
            } else {
                120
            };
            let outcome = if spec.prompt.contains("block") {
                cancel.cancelled().await;
                RunOutcome {
                    cancelled: true,
                    ..RunOutcome::default()
                }
            } else {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(delay)) => {
                        std::fs::write(&spec.output_path, format!("echo:{}", spec.prompt)).expect("output");
                        let code = if spec.prompt.contains("fail") { 3 } else { 0 };
                        RunOutcome { exit_code: Some(code), ..RunOutcome::default() }
                    }
                    () = cancel.cancelled() => RunOutcome { cancelled: true, ..RunOutcome::default() },
                }
            };
            self.running.fetch_sub(1, Ordering::SeqCst);
            outcome
        })
    }
}

fn manager(
    dir: &Path,
    observers: Vec<Arc<dyn TaskObserver>>,
) -> (Arc<TaskManager>, Arc<FakeRunner>) {
    let runner = Arc::new(FakeRunner::default());
    let manager = TaskManager::new(TaskManagerOptions {
        state: StateDir::new(dir.join("state")),
        settings: TaskSettings::default(),
        runner: Arc::clone(&runner) as Arc<dyn TaskRunner>,
        isolation: None,
        observers,
        hooks: HookChain::default(),
        depth: 0,
    });
    (manager, runner)
}

fn member(dir: &Path, name: &str, task: &str) -> MemberSpec {
    MemberSpec {
        name: name.to_owned(),
        request: SpawnRequest {
            agent: bundled_agents().remove(0),
            task: task.to_owned(),
            context: None,
            name: None,
            choice: ModelChoice::default(),
            cwd: dir.to_path_buf(),
            background: true,
            isolated: false,
            group: None,
            pane: None,
            quiet: false,
            owner_session: None,
            root_session: None,
        },
    }
}

type Inbox = Arc<Mutex<Vec<String>>>;

/// Fires once per accepted notice, so a test awaits the notice itself instead of polling the inbox.
/// The sender lives beside the inbox and is dropped with the test's scope.
#[derive(Clone, Default)]
struct NoticeSignal(Arc<tokio::sync::Notify>);

/// A notifier that also signals each accepted notice. The signal fires only AFTER the push, so a
/// woken test always observes the message it was waiting for.
fn signalling_notifier() -> (Notifier, Inbox, NoticeSignal) {
    let inbox: Inbox = Arc::default();
    let signal = NoticeSignal::default();
    let sink = Arc::clone(&inbox);
    let wake = signal.clone();
    let notifier: Notifier = Arc::new(move |kind: &str, text: &str, _details| {
        assert_eq!(kind, maho_team::UPDATE_MESSAGE_TYPE);
        lock(&sink).push(text.to_owned());
        wake.0.notify_waiters();
        Ok(())
    });
    (notifier, inbox, signal)
}

/// Fires when the manager records a task as cancelled. The observer hook runs after the manager has
/// saved the status, so a woken test reads the committed value rather than racing the write.
#[derive(Clone, Default)]
struct CancelledSignal(Arc<tokio::sync::Notify>);

impl CancelledSignal {
    async fn wait(&self) {
        let waited = tokio::time::timeout(Duration::from_secs(5), self.0.notified()).await;
        assert!(waited.is_ok(), "timed out waiting for a cancellation");
    }
}

impl TaskObserver for CancelledSignal {
    fn finished_in(&self, record: &TaskRecord, _state: &StateDir) {
        if record.status == TaskStatus::Cancelled {
            self.0.notify_waiters();
        }
    }
}

/// Awaits `check` against the inbox, waking on each accepted notice instead of polling.
async fn await_notice(signal: &NoticeSignal, what: &str, check: impl Fn() -> bool) {
    for _ in 0..64 {
        if check() {
            return;
        }
        let waited = tokio::time::timeout(Duration::from_secs(5), signal.0.notified()).await;
        if waited.is_err() {
            assert!(check(), "timed out waiting for {what}");
            return;
        }
    }
    assert!(check(), "gave up waiting for {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parallel_limit_updates_and_settled_notice() {
    let dir = tempfile::tempdir().expect("tmp");
    let (manager, runner) = manager(dir.path(), Vec::new());
    let (notifier, inbox, signal) = signalling_notifier();
    let limits = TeamLimits {
        max_parallel: 2,
        ..TeamLimits::default()
    };
    let members = vec![
        member(dir.path(), "a", "slow a"),
        member(dir.path(), "b", "slow b"),
        member(dir.path(), "c", "fail c"),
    ];
    let team = Team::create(
        Arc::clone(&manager),
        "crew",
        Some("shared goal".into()),
        members,
        limits,
        Some(notifier),
    )
    .expect("create");
    let file = team.file();
    assert_eq!(
        file.members[2].task_id, None,
        "third member waits for a slot"
    );
    let record = manager.get("crew-a").expect("member task");
    assert_eq!(record.group.as_deref(), Some("crew"));
    assert!(record.quiet && record.background);
    await_notice(&signal, "settled notice", || {
        lock(&inbox)
            .iter()
            .any(|t| t.contains("all 3 members finished"))
    })
    .await;
    assert_eq!(runner.peak.load(Ordering::SeqCst), 2);
    let updates = lock(&inbox).clone();
    assert_eq!(
        updates
            .iter()
            .filter(|t| t.starts_with("[team crew] "))
            .count(),
        4,
        "{updates:?}"
    );
    assert!(
        updates
            .iter()
            .any(|t| t.contains("c failed (task crew-c, run 1)")),
        "{updates:?}"
    );
    assert!(
        updates.iter().any(|t| t.contains("Progress: 3/3 done")),
        "{updates:?}"
    );
    assert!(
        updates.iter().any(|t| t.contains("2 succeeded, 1 not")),
        "{updates:?}"
    );
    assert!(
        lock(&runner.prompts)
            .iter()
            .all(|p| p.starts_with("# Context\nshared goal"))
    );
    assert_eq!(team.file().status, TeamStatus::Settled);
    let persisted = maho_team::team::load_file(&manager.state().root, "crew").expect("file");
    assert_eq!(persisted.members.len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_collects_without_notices_and_messages_resume() {
    let dir = tempfile::tempdir().expect("tmp");
    let (manager, runner) = manager(dir.path(), Vec::new());
    let (notifier, inbox, signal) = signalling_notifier();
    let members = vec![
        member(dir.path(), "a", "slow a"),
        member(dir.path(), "b", "quick b"),
    ];
    let team = Team::create(
        Arc::clone(&manager),
        "pair",
        None,
        members,
        TeamLimits::default(),
        Some(notifier),
    )
    .expect("create");
    // Queued while busy.
    let lines = team.message("a", "also check tests").expect("message");
    assert!(lines[0].contains("queued (1 waiting"), "{lines:?}");
    let text = team
        .wait(None, Duration::from_secs(10), &CancelToken::default())
        .await
        .expect("wait");
    assert!(text.starts_with("team pair: 2/2 done"), "{text}");
    assert!(
        text.contains("echo:also check tests"),
        "queued message ran as a follow-up: {text}"
    );
    assert_eq!(manager.get("pair-a").expect("a").runs, 2);

    // Idle member: delivered at once, same session, one update for the new run.
    let session = manager.get("pair-b").expect("b").session_id;
    let lines = team.message("b", "one more thing").expect("message");
    assert!(
        lines[0].contains("delivered (task pair-b, run 2 started)"),
        "{lines:?}"
    );
    await_notice(&signal, "update for b's second run", || {
        lock(&inbox)
            .iter()
            .any(|t| t.contains("b completed (task pair-b, run 2)"))
    })
    .await;
    // Ordering, not elapsed time, proves the suppressed notice never arrived: the driver emits
    // notices in pass order, so the first update after the collected wait is this run's. A
    // suppressed notice for the collected run would occupy index 0.
    let first = lock(&inbox)[0].clone();
    assert!(
        first.contains("b completed (task pair-b, run 2)"),
        "no notice from the collected run precedes it: {first:?}"
    );
    let record = manager.get("pair-b").expect("b");
    assert_eq!(record.session_id, session);
    assert_eq!(manager.state().read_output("pair-b"), "echo:one more thing");
    assert!(lock(&runner.prompts).contains(&"one more thing".to_owned()));
    assert!(team.message("nobody", "x").is_err());
    let listing = team.describe(1, None);
    assert!(
        listing.contains("- b: idle (completed), task pair-b"),
        "{listing}"
    );
    assert!(listing.contains("    | echo:one more thing"), "{listing}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_stop_and_limits() {
    let dir = tempfile::tempdir().expect("tmp");
    let cancelled = CancelledSignal::default();
    let watcher = Arc::new(cancelled.clone());
    let (manager, _runner) = manager(dir.path(), vec![watcher]);
    let members = vec![
        member(dir.path(), "a", "block a"),
        member(dir.path(), "b", "block b"),
    ];
    let limits = TeamLimits {
        max_parallel: 1,
        ..TeamLimits::default()
    };
    let team = Team::create(
        Arc::clone(&manager),
        "hold",
        None,
        members,
        limits.clone(),
        None,
    )
    .expect("create");
    let text = team
        .wait(None, Duration::from_millis(300), &CancelToken::default())
        .await
        .expect("wait");
    assert!(
        text.starts_with("team hold: 0/2 done, timed out after"),
        "{text}"
    );
    assert!(text.contains("(b is pending)"), "{text}");
    assert!(
        Team::create(
            Arc::clone(&manager),
            "hold",
            None,
            vec![member(dir.path(), "x", "y")],
            limits.clone(),
            None
        )
        .unwrap_err()
        .contains("still running")
    );
    let lines = team.stop(None);
    assert!(lines[0].starts_with("a: Cancelling"), "{lines:?}");
    assert_eq!(lines[1], "b: not started");
    assert_eq!(team.file().status, TeamStatus::Stopped);
    cancelled.wait().await;
    assert_eq!(
        manager.get("hold-a").expect("a").status,
        TaskStatus::Cancelled
    );
    assert!(team.message("a", "hello").unwrap_err().contains("stopped"));
    assert!(manager.get("hold-b").is_none());

    let too_many: Vec<MemberSpec> = (0..3)
        .map(|i| member(dir.path(), &format!("m{i}"), "x"))
        .collect();
    let small = TeamLimits {
        max_members: 2,
        ..TeamLimits::default()
    };
    assert!(
        Team::create(Arc::clone(&manager), "big", None, too_many, small, None)
            .unwrap_err()
            .contains("at most 2")
    );
    assert!(
        Team::create(
            Arc::clone(&manager),
            "Bad",
            None,
            vec![member(dir.path(), "a", "x")],
            limits.clone(),
            None
        )
        .is_err()
    );
    let dup = vec![member(dir.path(), "a", "x"), member(dir.path(), "a", "y")];
    assert!(
        Team::create(Arc::clone(&manager), "dup", None, dup, limits, None)
            .unwrap_err()
            .contains("Duplicate")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wall_clock_limit_stops_the_team() {
    let dir = tempfile::tempdir().expect("tmp");
    let (manager, _runner) = manager(dir.path(), Vec::new());
    let (notifier, inbox, signal) = signalling_notifier();
    let limits = TeamLimits {
        max_wall_clock: Duration::from_millis(200),
        ..TeamLimits::default()
    };
    let team = Team::create(
        Arc::clone(&manager),
        "late",
        None,
        vec![member(dir.path(), "a", "block")],
        limits,
        Some(notifier),
    )
    .expect("create");
    // The notice is emitted by the same driver pass that sets `TimedOut`, so awaiting the notice
    // covers the status transition too.
    await_notice(&signal, "wall-clock notice", || {
        lock(&inbox).iter().any(|t| t.contains("wall-clock limit"))
    })
    .await;
    assert_eq!(team.file().status, TeamStatus::TimedOut);
}

/// A refused update used to be dropped: the driver printed nothing, the lead never saw the
/// notice, and nothing retried it. The driver must hand it over again on a later pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_update_is_retried_until_delivered() {
    let dir = tempfile::tempdir().expect("tmp");
    let (manager, _runner) = manager(dir.path(), Vec::new());
    let inbox: Inbox = Arc::default();
    let sink = Arc::clone(&inbox);
    let refusals = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let budget = Arc::clone(&refusals);
    let signal = NoticeSignal::default();
    let wake = signal.clone();
    let notifier: Notifier = Arc::new(move |kind: &str, text: &str, _details| {
        assert_eq!(kind, maho_team::UPDATE_MESSAGE_TYPE);
        // Refuse the first two attempts of every message, then accept.
        if budget.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            return Err("session is switching".to_owned());
        }
        lock(&sink).push(text.to_owned());
        wake.0.notify_waiters();
        Ok(())
    });
    let limits = TeamLimits {
        max_wall_clock: Duration::from_millis(200),
        ..TeamLimits::default()
    };
    let _team = Team::create(
        Arc::clone(&manager),
        "retry",
        None,
        vec![member(dir.path(), "a", "block")],
        limits,
        Some(notifier),
    )
    .expect("create");
    await_notice(&signal, "retried notice", || {
        lock(&inbox).iter().any(|t| t.contains("wall-clock limit"))
    })
    .await;
}

#[cfg(unix)]
fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).expect("write");
    let mut perms = std::fs::metadata(path).expect("meta").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(path, perms).expect("chmod");
}

/// A fake `tmux` that logs its arguments and answers `split-window` with `%7`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panes_with_fake_tmux() {
    let dir = tempfile::tempdir().expect("tmp");
    let log = dir.path().join("calls");
    let tmux = dir.path().join("tmux");
    executable(
        &tmux,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in split-window) echo %7 ;; esac\n",
            log.display()
        ),
    );
    let backend = TmuxBackend {
        program: tmux,
        prefix_args: Vec::new(),
        lead_pane: Some("%0".into()),
    };
    let settings = PaneSettings {
        linger: Duration::ZERO,
        placement: Placement::Split,
        mode: PaneMode::Auto,
        ..PaneSettings::default()
    };
    let observer = Arc::new(PaneObserver::new(Some(Arc::new(backend)), settings));
    let (manager, _runner) = manager(
        dir.path(),
        vec![Arc::clone(&observer) as Arc<dyn TaskObserver>],
    );
    let team = Team::create(
        Arc::clone(&manager),
        "view",
        None,
        vec![member(dir.path(), "a", "quick")],
        TeamLimits::default(),
        None,
    )
    .expect("create");
    team.wait(None, Duration::from_secs(5), &CancelToken::default())
        .await
        .expect("wait");
    await_log_line(&log, "kill-pane -t %7").await;
    let calls = std::fs::read_to_string(&log).expect("calls");
    let first = calls.lines().next().expect("split");
    assert!(
        first.starts_with(&format!(
            "split-window -d -h -c {} -t %0 -P -F #{{pane_id}} -- /bin/sh -c printf",
            dir.path().display()
        )),
        "{calls}"
    );
    assert!(
        first.contains("tail -n +1 -F")
            && first.contains("view-a.log")
            && first.contains("kill -0"),
        "{calls}"
    );
    assert!(
        calls.contains("select-pane -t %7 -T team view · task view-a"),
        "{calls}"
    );
}

/// Awaits a line in a file another process writes. The writer emits no in-process signal, so this
/// is the one bounded external wait the plan permits; it wakes on file growth instead of a fixed
/// cadence, and the timeout bounds a writer that never produces the line.
async fn await_log_line(path: &Path, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {needle:?} in {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Awaits a condition on `tmux` output. tmux is an external server with no in-process signal, so
/// this polls it; the bounded timeout keeps a stuck server from hanging the suite.
async fn await_tmux(tmux: &impl Fn(&[&str]) -> String, args: &[&str], done: impl Fn(&str) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if done(&tmux(args)) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for tmux {args:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(unix)]
fn real_tmux() -> Option<PathBuf> {
    let path = std::env::var("PATH").ok();
    maho_team::mux::which("tmux", path.as_deref())
}

/// With a real tmux server: the member's pane appears, shows its output, and closes when the
/// member settles.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panes_with_real_tmux() {
    let Some(program) = real_tmux() else {
        eprintln!("tmux not on PATH; skipping");
        return;
    };
    let dir = tempfile::tempdir().expect("tmp");
    let socket = format!("maho-team-test-{}", std::process::id());
    let tmux = |args: &[&str]| -> String {
        let output = std::process::Command::new(&program)
            .arg("-L")
            .arg(&socket)
            .args(args)
            .output()
            .expect("tmux");
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    tmux(&[
        "new-session",
        "-d",
        "-s",
        "lead",
        "-x",
        "200",
        "-y",
        "50",
        "sleep 60",
    ]);
    let lead = tmux(&["list-panes", "-t", "lead", "-F", "#{pane_id}"])
        .trim()
        .to_owned();
    assert!(lead.starts_with('%'), "lead pane {lead:?}");
    let backend = TmuxBackend {
        program: program.clone(),
        prefix_args: vec!["-L".into(), socket.clone()],
        lead_pane: Some(lead),
    };
    let settings = PaneSettings {
        linger: Duration::from_millis(2000),
        ..PaneSettings::default()
    };
    let observer = Arc::new(PaneObserver::new(Some(Arc::new(backend)), settings));
    let (manager, _runner) = manager(
        dir.path(),
        vec![Arc::clone(&observer) as Arc<dyn TaskObserver>],
    );
    let members = vec![
        member(dir.path(), "a", "slow visible a"),
        member(dir.path(), "b", "slow visible b"),
    ];
    let team = Team::create(
        Arc::clone(&manager),
        "live",
        None,
        members,
        TeamLimits::default(),
        None,
    )
    .expect("create");
    await_tmux(&tmux, &["list-panes", "-a", "-F", "#{pane_id}"], |out| {
        out.lines().count() == 3
    })
    .await;
    let pane = observer.pane_of("live-a").expect("pane of a").id;
    await_tmux(&tmux, &["capture-pane", "-p", "-t", &pane], |out| {
        out.contains("echo:slow visible a")
    })
    .await;
    let title = tmux(&["display-message", "-p", "-t", &pane, "#{pane_title}"]);
    assert!(title.contains("team live"), "{title}");
    team.wait(None, Duration::from_secs(10), &CancelToken::default())
        .await
        .expect("wait");
    await_tmux(&tmux, &["list-panes", "-a", "-F", "#{pane_id}"], |out| {
        out.lines().count() == 1
    })
    .await;
    tmux(&["kill-server"]);
}
