//! Acceptance tests of docs/cleanroom/specs/side-panel.md §9 (numbered `t01` .. `t26`).

use maho_ext_api::Extension;
use maho_ext_api::{
    EventBus, ExecResult, ExtensionApi, ExtensionMode, ExtensionRuntime, ExtensionSessionProfile,
    FlagValue, LoadedExtension, NotificationType, SourceInfo,
};
use maho_side_panel::extension::{Decision, LoadedConfig, decide, flag_override};
use maho_side_panel::git::{self, ChangedFile};
use maho_side_panel::layout::{self, MountMode};
use maho_side_panel::links::Link;
use maho_side_panel::panel::{ExecError, HostFuture, Panel, PanelEnv, PanelHost};
use maho_side_panel::sections::{self, PanelView, SessionFacts, plain};
use maho_side_panel::settings::{Settings, Width};
use maho_side_panel::tasks::{Status, TaskBoard, TaskUpdate};
use maho_side_panel::text::{fit, fit_rows, sanitize, visible_width};
use maho_side_panel::tools::ToolLog;
use maho_side_panel::viewer::{KeyOutcome, Viewer, ViewerRow};
use maho_side_panel::{DIFF_COMMAND, FLAG_NAME, SidePanelExtension};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const BOUND: Duration = Duration::from_secs(5);

// ---- test host -------------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Event {
    Repaint,
    Mount(MountMode),
    Unmount,
    Notify(String, NotificationType),
    Viewer(Viewer),
    Pick(Vec<String>),
}

type GitFn = Box<dyn Fn(&[String]) -> Result<ExecResult, ExecError> + Send + Sync>;

struct TestHost {
    clock: AtomicU64,
    git: GitFn,
    git_calls: Mutex<Vec<Vec<String>>>,
    gate: Mutex<Option<oneshot::Receiver<()>>>,
    entered: Mutex<Option<oneshot::Sender<()>>>,
    events: Mutex<Vec<Event>>,
    repaints: mpsc::UnboundedSender<()>,
    pick_answer: Mutex<Option<String>>,
}

impl TestHost {
    fn new(git: GitFn) -> (Arc<Self>, mpsc::UnboundedReceiver<()>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                clock: AtomicU64::new(0),
                git,
                git_calls: Mutex::new(Vec::new()),
                gate: Mutex::new(None),
                entered: Mutex::new(None),
                events: Mutex::new(Vec::new()),
                repaints: tx,
                pick_answer: Mutex::new(None),
            }),
            rx,
        )
    }

    fn events(&self) -> Vec<Event> {
        self.events.lock().expect("events").clone()
    }

    fn push(&self, event: Event) {
        self.events.lock().expect("events").push(event);
    }

    fn notices(&self) -> Vec<String> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Notify(m, _) => Some(m),
                _ => None,
            })
            .collect()
    }

    fn viewers(&self) -> Vec<Viewer> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Viewer(v) => Some(v),
                _ => None,
            })
            .collect()
    }

    fn status_calls(&self) -> usize {
        self.git_calls
            .lock()
            .expect("calls")
            .iter()
            .filter(|args| args.first().map(String::as_str) == Some("status"))
            .count()
    }
}

impl PanelHost for TestHost {
    fn now_ms(&self) -> u64 {
        self.clock.load(Ordering::SeqCst)
    }

    fn git(
        &self,
        args: Vec<String>,
        _cwd: PathBuf,
        _timeout_ms: u64,
    ) -> HostFuture<Result<ExecResult, ExecError>> {
        self.git_calls.lock().expect("calls").push(args.clone());
        let result = (self.git)(&args);
        let gate = self.gate.lock().expect("gate").take();
        let entered = self.entered.lock().expect("entered").take();
        Box::pin(async move {
            if let Some(entered) = entered {
                let _ = entered.send(());
            }
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            result
        })
    }

    fn repaint(&self) {
        self.push(Event::Repaint);
        let _ = self.repaints.send(());
    }

    fn mount(&self, mode: MountMode) {
        self.push(Event::Mount(mode));
    }

    fn unmount(&self) {
        self.push(Event::Unmount);
    }

    fn notify(&self, message: &str, kind: NotificationType) {
        self.push(Event::Notify(message.into(), kind));
    }

    fn open_viewer(&self, viewer: Viewer) {
        self.push(Event::Viewer(viewer));
    }

    fn pick(&self, _title: String, options: Vec<String>) -> HostFuture<Option<String>> {
        self.push(Event::Pick(options));
        let answer = self.pick_answer.lock().expect("pick").clone();
        Box::pin(async move { answer })
    }

    fn debug(&self, _message: &str) {}
}

fn ok(stdout: &str, code: i32) -> Result<ExecResult, ExecError> {
    Ok(ExecResult {
        stdout: stdout.into(),
        stderr: String::new(),
        code,
        killed: false,
    })
}

/// A git stub for a repository at `/repo` with the given status/diff outputs.
fn repo_git(status: &'static str, diff: String) -> GitFn {
    Box::new(move |args: &[String]| {
        let first = args.first().map(String::as_str);
        match first {
            Some("rev-parse") => ok("/repo\n", 0),
            Some("status") => ok(status, 0),
            Some("diff") if args.iter().any(|a| a == "--numstat") => ok("", 0),
            Some("--literal-pathspecs") => ok(&diff, 0),
            Some("--version") => ok("git version 2\n", 0),
            _ => ok("", 0),
        }
    })
}

fn env(agent_dir: &Path) -> PanelEnv {
    PanelEnv {
        cwd: PathBuf::from("/repo"),
        home: None,
        agent_dir: agent_dir.to_path_buf(),
        session_id: Some("s1".into()),
        task_dir: None,
        goal_file: None,
        fullscreen: true,
        links_supported: true,
        memory: None,
        usage: None,
    }
}

fn enabled() -> Settings {
    Settings {
        enabled: true,
        ..Settings::default()
    }
}

fn task(id: &str, status: &str) -> TaskUpdate {
    TaskUpdate::from_record(&json!({"id": id, "status": status, "owner_session": "s1"}))
        .expect("record")
}

fn config(side_panel: Value) -> LoadedConfig {
    LoadedConfig {
        view: json!({ "side_panel": side_panel }),
        side_panel_layers: vec![side_panel],
    }
}

fn test_api() -> ExtensionApi {
    ExtensionApi::new(
        LoadedExtension::new("<builtin:side-panel>", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    )
}

fn no_control(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

// ---- 1-7: enabling, settings, layout ---------------------------------------------------------

#[test]
fn t01_default_config_without_flag_registers_nothing_and_stays_dark() {
    let mut api = test_api();
    SidePanelExtension::default().register(&mut api);
    assert!(api.registered.flags.iter().any(|f| f.name == FLAG_NAME));
    assert!(
        !api.registered
            .commands
            .iter()
            .any(|c| c.name == DIFF_COMMAND)
    );
    assert_eq!(flag_override(&api), None);
    let decision = decide(ExtensionMode::Tui, true, &LoadedConfig::default(), None);
    assert_eq!(decision, Decision::Off);
}

#[test]
fn t02_flag_wins_over_config_in_both_directions() {
    let off = config(json!({"enabled": false}));
    let on = config(json!({"enabled": true}));
    assert!(matches!(
        decide(ExtensionMode::Tui, true, &off, Some(true)),
        Decision::On(_)
    ));
    assert_eq!(
        decide(ExtensionMode::Tui, true, &on, Some(false)),
        Decision::Off
    );

    let mut api = test_api();
    SidePanelExtension::default().register(&mut api);
    api.set_flag(FLAG_NAME, FlagValue::Boolean(false));
    assert_eq!(flag_override(&api), Some(false));
}

#[test]
fn t03_each_invalid_setting_names_its_field() {
    let cases = [
        (json!({"width": 23}), "side_panel.width"),
        (json!({"width": 161}), "side_panel.width"),
        (json!({"width": "9%"}), "side_panel.width"),
        (json!({"width": "5%"}), "side_panel.width"),
        (json!({"width": "100%"}), "side_panel.width"),
        (json!({"min_columns": 59}), "side_panel.min_columns"),
        (
            json!({"usage_poll_seconds": 3601}),
            "side_panel.usage_poll_seconds",
        ),
        (json!({"colour": true}), "side_panel.colour"),
        (
            json!({"sections": {"extra": true}}),
            "side_panel.sections.extra",
        ),
    ];
    for (value, field) in cases {
        let error = Settings::from_value(Some(&value)).expect_err("invalid");
        assert_eq!(error.field, field, "{value}");
        match decide(ExtensionMode::Tui, true, &config(value), Some(true)) {
            Decision::Invalid(error) => assert_eq!(error.field, field),
            other => panic!("expected invalid, got {other:?}"),
        }
    }
    assert_eq!(Settings::from_value(None), Ok(Settings::default()));
}

#[test]
fn t04_effective_width() {
    assert_eq!(layout::effective_width(Width::Percent(26), 200), 52);
    assert_eq!(layout::effective_width(Width::Percent(26), 80), 32);
    assert_eq!(layout::effective_width(Width::Columns(200), 0), 80);
    assert_eq!(layout::effective_width(Width::Columns(200), 100), 40);
}

#[test]
fn t05_column_hidden_below_min_columns_and_shown_again_without_remount() {
    let home = tempfile::tempdir().expect("tempdir");
    let (host, _rx) = TestHost::new(repo_git("", String::new()));
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.start();
    assert!(panel.column_visible(130));
    assert!(!panel.column_visible(100));
    assert!(panel.is_mounted());
    assert!(panel.column_visible(120));
    let mounts = host
        .events()
        .iter()
        .filter(|e| matches!(e, Event::Mount(_)))
        .count();
    assert_eq!(mounts, 1);
}

#[test]
fn t06_block_mode_shows_the_notice_once_per_user() {
    let home = tempfile::tempdir().expect("tempdir");
    let mut regular = env(home.path());
    regular.fullscreen = false;
    for round in 0..2 {
        let (host, _rx) = TestHost::new(repo_git("", String::new()));
        let panel = Panel::new(host.clone(), enabled(), regular.clone());
        panel.start();
        assert_eq!(panel.mount_mode(), Some(MountMode::Block));
        assert_eq!(
            host.notices().len(),
            usize::from(round == 0),
            "round {round}"
        );
        assert!(layout::notice_marker_path(home.path()).exists());
        panel.set_fullscreen(true);
        assert_eq!(panel.mount_mode(), Some(MountMode::Column));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(layout::notice_marker_path(home.path()))
            .expect("marker")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let mut unwritable = env(&home.path().join("missing").join("home"));
    unwritable.fullscreen = false;
    let (host, _rx) = TestHost::new(repo_git("", String::new()));
    let panel = Panel::new(host.clone(), enabled(), unwritable);
    panel.start();
    assert!(host.notices().is_empty());
    assert!(panel.is_mounted());
}

#[test]
fn t07_print_mode_or_no_ui_stays_dark_without_warning() {
    let on = config(json!({"enabled": true}));
    for mode in [
        ExtensionMode::Print,
        ExtensionMode::Json,
        ExtensionMode::Rpc,
    ] {
        assert_eq!(decide(mode, true, &on, Some(true)), Decision::Off);
    }
    assert_eq!(
        decide(ExtensionMode::Tui, false, &on, Some(true)),
        Decision::Off
    );
    let bad = config(json!({"width": 1}));
    assert_eq!(
        decide(ExtensionMode::Print, true, &bad, Some(true)),
        Decision::Off
    );
}

// ---- 8-11: rendering --------------------------------------------------------------------------

#[test]
fn t08_rows_are_fitted_to_the_width() {
    assert_eq!(fit("xy", 4), "xy  ");
    assert_eq!(fit("abcdefg", 4), "abc…");
    assert_eq!(fit("abcdefg", 1), "a");
    assert_eq!(visible_width(&fit("abcdefg", 1)), 1);
    assert!(fit_rows(&["abc".to_owned()], 0).is_empty());
}

#[test]
fn t09_control_bytes_are_removed_and_wide_characters_count_two() {
    let raw = "a\x1b]8;;http://x\x07b\x1b[31mc\nd\re\u{202E}f\u{2066}g\x07";
    let clean = sanitize(raw);
    assert!(no_control(&clean));
    assert!(!clean.contains('\u{202E}') && !clean.contains('\u{2066}'));
    let row = fit(&clean, 40);
    assert!(no_control(&row));
    assert_eq!(visible_width(&row), 40);
    assert_eq!(visible_width("日本"), 4);
    assert_eq!(fit("日本語", 5), "日本…");
}

fn empty_view() -> PanelView {
    PanelView {
        location: "/repo".into(),
        ..PanelView::default()
    }
}

#[test]
fn t10_separators_between_sections() {
    let mut view = empty_view();
    view.sections.session = false;
    assert_eq!(sections::render(&view, 40, &plain).len(), 1);

    let mut view = empty_view();
    view.session.model_id = Some("prov/model-x".into());
    view.tools.record("read", &json!({"path": "a.rs"}));
    view.files = Some(git::merge(" M a.rs\0", "", ""));
    view.tasks.apply(task("t1", "running"));
    let rows = sections::render(&view, 40, &plain);
    let blanks = rows.iter().filter(|r| r.trim().is_empty()).count();
    assert_eq!(blanks, 4);
    assert!(rows.iter().all(|r| visible_width(r) == 40));
}

#[test]
fn t11_disabled_section_leaves_no_gap() {
    let mut view = empty_view();
    view.tools.record("read", &json!({"path": "a.rs"}));
    view.files = Some(git::merge(" M a.rs\0", "", ""));
    view.sections.tools = false;
    let rows = sections::compose(&view, 40, &plain);
    assert!(!rows.iter().any(|r| r.starts_with("TOOLS")));
    for pair in rows.windows(2) {
        assert!(!(pair[0].is_empty() && pair[1].is_empty()), "{rows:?}");
    }
    assert!(!rows[0].is_empty());
}

// ---- 12-17: data sections ---------------------------------------------------------------------

#[test]
fn t12_running_tasks_are_never_evicted() {
    let mut board = TaskBoard::default();
    for i in 0..13 {
        board.apply(task(&format!("t{i}"), "running"));
    }
    assert_eq!(board.tasks().len(), 13);
    board.apply(task("t0", "completed"));
    assert_eq!(board.tasks().len(), 12);
    assert!(board.get("t0").is_none());
    assert!(board.tasks().iter().all(|t| t.status == Status::Running));
}

#[test]
fn t13_cost_is_counted_once() {
    let mut board = TaskBoard::default();
    let done = TaskUpdate::from_record(&json!({"id": "t", "status": "completed", "cost": 0.5}))
        .expect("record");
    board.apply(task("t", "running"));
    board.apply(done.clone());
    board.apply(done);
    assert!((board.spend - 0.5).abs() < f64::EPSILON);
}

#[test]
fn t14_only_tasks_of_the_current_session_are_listed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let write = |id: &str, owner: &str| {
        std::fs::write(
            dir.path().join(format!("{id}.json")),
            json!({"id": id, "status": "running", "owner_session": owner}).to_string(),
        )
        .expect("write");
    };
    write("mine", "s1");
    write("theirs", "s2");
    let home = tempfile::tempdir().expect("tempdir");
    let (host, _rx) = TestHost::new(repo_git("", String::new()));
    let mut panel_env = env(home.path());
    panel_env.task_dir = Some(dir.path().to_path_buf());
    let panel = Panel::new(host.clone(), enabled(), panel_env.clone());
    panel.start();
    panel.refresh_tasks();
    let ids: Vec<String> = panel.board().tasks().iter().map(|t| t.id.clone()).collect();
    assert_eq!(ids, vec!["mine".to_owned()]);

    panel_env.session_id = None;
    let (host, _rx) = TestHost::new(repo_git("", String::new()));
    let panel = Panel::new(host, enabled(), panel_env);
    panel.start();
    panel.refresh_tasks();
    assert!(panel.board().tasks().is_empty());
}

#[test]
fn t15_interrupted_task_is_parked_without_duration() {
    let mut view = empty_view();
    view.now_ms = 10_000;
    view.tasks.apply(
        TaskUpdate::from_record(&json!({
            "id": "t", "description": "fix it", "status": "interrupted", "started_at_ms": 5_000
        }))
        .expect("record"),
    );
    assert_eq!(view.tasks.counts().parked, 1);
    assert_eq!(view.tasks.tasks()[0].status, Status::Parked);
    let rows = sections::compose(&view, 60, &plain);
    let row = rows
        .iter()
        .find(|r| r.contains("fix it"))
        .expect("task row");
    assert!(row.contains(Status::Parked.glyph()));
    assert!(!row.contains("5s"), "{row}");
    assert!(rows.iter().any(|r| r.contains("1 parked")));
}

#[test]
fn t16_tool_log_keeps_24_shows_6() {
    let mut log = ToolLog::default();
    for i in 0..30 {
        log.record("bash", &json!({"command": format!("echo {i}")}));
    }
    assert_eq!(log.stored().len(), 24);
    assert_eq!(log.shown().len(), 6);
    assert_eq!(log.earlier(), 18);
    let mut view = empty_view();
    view.tools = log.clone();
    let rows = sections::compose(&view, 60, &plain);
    assert!(rows.iter().any(|r| r == "TOOLS +18 earlier"));
    assert!(rows.iter().any(|r| r == "bash echo 29"));
    log.clear();
    assert!(log.is_empty());
}

#[test]
fn t17_rename_untracked_and_binary_counts() {
    let status = "R  b.txt\0a.txt\0?? new.txt\0M  bin.dat\0";
    let staged = "4\t0\t\0a.txt\0b.txt\0-\t-\tbin.dat\0";
    let unstaged = "1\t2\tb.txt\0";
    let files = git::merge(status, staged, unstaged);
    let find = |p: &str| files.iter().find(|f| f.path == p).cloned().expect(p);
    let renamed: ChangedFile = find("b.txt");
    assert_eq!(renamed.from.as_deref(), Some("a.txt"));
    assert_eq!(renamed.counts_label(), " +5/-2");
    assert_eq!(find("new.txt").counts_label(), "");
    assert_eq!(find("bin.dat").counts_label(), " +0/-0");
}

// ---- 18-20: refresh lifecycle -----------------------------------------------------------------

#[tokio::test]
async fn t18_git_refresh_is_throttled_unless_forced() {
    let home = tempfile::tempdir().expect("tempdir");
    let (host, _rx) = TestHost::new(repo_git(" M a.rs\0", String::new()));
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.start();
    panel.refresh_git(false).await;
    host.clock.store(1_500, Ordering::SeqCst);
    panel.refresh_git(false).await;
    assert_eq!(host.status_calls(), 1);
    panel.refresh_git(true).await;
    assert_eq!(host.status_calls(), 2);
    assert_eq!(panel.git_runs(), 2);
}

#[tokio::test(start_paused = true)]
async fn t19_running_task_repaints_each_second_until_terminal() {
    let home = tempfile::tempdir().expect("tempdir");
    let (host, mut rx) = TestHost::new(repo_git("", String::new()));
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.start();
    panel.apply_task(task("t", "running"));
    assert!(panel.ticker_running());
    let before = tokio::time::Instant::now();
    for _ in 0..2 {
        tokio::time::timeout(BOUND, rx.recv())
            .await
            .expect("tick repaint")
            .expect("channel");
    }
    assert!(tokio::time::Instant::now() - before >= Duration::from_secs(2));
    panel.apply_task(task("t", "completed"));
    assert!(!panel.ticker_running());
    while rx.try_recv().is_ok() {}
    tokio::time::advance(Duration::from_secs(3)).await;
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn t20_late_git_result_after_shutdown_is_discarded() {
    let home = tempfile::tempdir().expect("tempdir");
    let (host, _rx) = TestHost::new(repo_git(" M a.rs\0", String::new()));
    let (release, gate) = oneshot::channel();
    let (entered_tx, entered) = oneshot::channel();
    *host.gate.lock().expect("gate") = Some(gate);
    *host.entered.lock().expect("entered") = Some(entered_tx);
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.start();
    let background = Arc::clone(&panel);
    let job = tokio::spawn(async move { background.refresh_git(true).await });
    tokio::time::timeout(BOUND, entered)
        .await
        .expect("git started")
        .expect("signal");
    panel.shutdown();
    let after_shutdown = host.events().len();
    release.send(()).expect("release");
    tokio::time::timeout(BOUND, job)
        .await
        .expect("job finished")
        .expect("join");
    let events = host.events();
    assert!(matches!(events.last(), Some(Event::Unmount)));
    assert_eq!(events.len(), after_shutdown);
    assert!(panel.files().is_none());
    assert!(!panel.is_mounted());
    assert!(panel.render(40, &plain).is_empty());
}

// ---- 21-24: actions ---------------------------------------------------------------------------

#[test]
fn t21_link_urls_round_trip_and_reject_foreign_or_malformed() {
    for path in [
        "dir with space/a#b.rs",
        "a/b/c",
        "x\x1by\x07z",
        "日本/ü.txt",
    ] {
        let link = Link::File(path.into());
        let url = link.url();
        assert!(!url.contains('\x1b') && !url.contains('\x07'), "{url}");
        assert!(!url.contains(' ') && !url.contains('#'));
        assert_eq!(Link::parse(&url), Some(link));
    }
    assert_eq!(Link::parse("mhc-panel:goal/current"), Some(Link::Goal));
    assert_eq!(Link::parse("mhc-panel:memory/current"), Some(Link::Memory));
    for url in [
        "https://example.com/a",
        "mhc-panel:bogus/x",
        "mhc-panel:file",
        "mhc-panel:file/",
        "mhc-panel:file/%zz",
        "mhc-panel:file/%4",
        "mhc-panel:file/%ff%fe",
        "mhc-panel:goal/other",
        "mhc-panel:memory/x",
    ] {
        assert_eq!(Link::parse(url), None, "{url}");
    }
}

#[tokio::test]
async fn t22_file_click_opens_capped_diff_and_stale_file_notices() {
    let home = tempfile::tempdir().expect("tempdir");
    let diff: String = (0..5_010).map(|i| format!("+line {i}\n")).collect();
    let (host, _rx) = TestHost::new(repo_git(" M a.rs\0", diff));
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.start();
    panel.refresh_git(true).await;
    let rows = panel.render(60, &plain);
    let row = rows.iter().find(|r| r.contains("a.rs")).expect("file row");
    let start = row.find("\x1b]8;;").expect("link") + 5;
    let end = start + row[start..].find("\x1b\\").expect("st");
    let link = Link::parse(&row[start..end]).expect("claimed");
    panel.open(link).await;
    let viewer = host.viewers().pop().expect("viewer");
    assert_eq!(viewer.title, "a.rs");
    assert_eq!(viewer.rows.len(), 5_001);
    assert_eq!(
        viewer.rows.last().map(|r| r.text.as_str()),
        Some("... 10 more lines")
    );

    panel.open(Link::File("gone.rs".into())).await;
    assert!(host.notices().iter().any(|n| n.contains("gone.rs")));
    assert_eq!(host.viewers().len(), 1);
}

#[tokio::test]
async fn t23_diff_command_notices_and_dismissal() {
    let home = tempfile::tempdir().expect("tempdir");

    // Panel off (not mounted in this session).
    let (host, _rx) = TestHost::new(repo_git(" M a.rs\0", String::new()));
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.diff_command().await;
    assert_eq!(host.notices().len(), 1);
    assert!(host.viewers().is_empty());

    // No changes.
    let (host, _rx) = TestHost::new(repo_git("", String::new()));
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.start();
    panel.diff_command().await;
    assert_eq!(host.notices().len(), 1);
    assert!(host.viewers().is_empty());

    // No exec available.
    let (host, _rx) = TestHost::new(Box::new(|_: &[String]| Err(ExecError::Unavailable)));
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.start();
    panel.diff_command().await;
    assert_eq!(host.notices().len(), 1);
    assert!(host.viewers().is_empty());

    // Dismissed picker.
    let (host, _rx) = TestHost::new(repo_git(" M a.rs\0", "+x\n".into()));
    let panel = Panel::new(host.clone(), enabled(), env(home.path()));
    panel.start();
    panel.diff_command().await;
    assert!(
        host.events()
            .iter()
            .any(|e| matches!(e, Event::Pick(o) if o == &[" M a.rs +0/-0".to_owned()]))
    );
    assert!(host.viewers().is_empty());
    assert!(host.notices().is_empty());

    // Picking opens the diff.
    *host.pick_answer.lock().expect("pick") = Some(" M a.rs +0/-0".into());
    panel.diff_command().await;
    assert_eq!(host.viewers().len(), 1);
}

#[test]
fn t24_viewer_geometry_scrolling_and_keys() {
    let rows: Vec<ViewerRow> = (0..50)
        .map(|i| ViewerRow::plain(format!("row {i}")))
        .collect();
    let mut viewer = Viewer::new("title", rows, 30);
    assert_eq!(viewer.height(), 21);
    assert_eq!(viewer.body_rows(), 17);
    let rendered = viewer.render(40, &|t: &str| t.to_owned());
    assert_eq!(rendered.len(), 21);
    assert!(rendered.iter().any(|r| r.contains("1-17/50")));
    assert_eq!(viewer.handle_input("\x1b[6~"), KeyOutcome::Scrolled);
    assert_eq!(viewer.handle_input("\x1b[<65;3;4M"), KeyOutcome::Scrolled);
    assert_eq!(viewer.scroll, 13);
    viewer.scroll_to(1_000);
    assert_eq!(viewer.scroll, 33);
    assert_eq!(viewer.handle_input("x"), KeyOutcome::Ignored);
    assert_eq!(viewer.scroll, 33);
    assert_eq!(viewer.handle_input("q"), KeyOutcome::Close);
    assert_eq!(viewer.handle_input("\x1b"), KeyOutcome::Close);
    assert_eq!(viewer.handle_input("\r"), KeyOutcome::Close);
}

// ---- 25-26: goal and context ------------------------------------------------------------------

#[test]
fn t25_goal_file_parsing_and_caching() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("goal.json");
    let mut reader = maho_side_panel::goal::GoalReader::default();

    std::fs::write(&path, r#"{"status":"dreaming","objective":"x"}"#).expect("write");
    assert!(reader.read(&path).is_none());
    std::fs::write(&path, "{not json").expect("write");
    assert!(reader.read(&path).is_none());

    let valid = r#"{"id":"g","status":"active","objective":"ship the panel","tokensUsed":10}"#;
    std::fs::write(&path, valid).expect("write");
    let goal = reader.read(&path).expect("goal");
    assert_eq!(goal.status, "active");
    let parses = reader.parses;
    assert!(reader.read(&path).is_some());
    assert_eq!(reader.parses, parses);

    let mut view = empty_view();
    view.goal = Some(goal);
    let rows = sections::compose(&view, 60, &plain);
    assert!(rows.iter().any(|r| r.starts_with("GOAL active")));
    assert!(rows.iter().any(|r| r.contains("ship the panel")));
}

#[test]
fn t26_context_without_token_count_shows_placeholder() {
    let mut view = empty_view();
    view.session = SessionFacts {
        context_window: Some(200_000),
        context_tokens: None,
        ..SessionFacts::default()
    };
    let rows = sections::compose(&view, 60, &plain);
    let heading = rows
        .iter()
        .find(|r| r.starts_with("CONTEXT"))
        .expect("context");
    assert!(!heading.starts_with("CONTEXT 0"), "{heading}");
    assert!(rows.iter().any(|r| r.contains("measuring")));
}
