use super::*;
use crate::store::PanelChildStatus;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Default)]
struct Readers {
    git_reads: AtomicUsize,
    memory_reads: AtomicUsize,
    children_reads: AtomicUsize,
    pending_git: Mutex<Option<tokio::sync::oneshot::Receiver<PanelGitStatus>>>,
}
impl PanelReaders for Readers {
    fn facts(&self, _: &Value, previous: &PanelHostFacts) -> PanelHostFacts {
        previous.clone()
    }
    fn find_git_root(&self, _: &str) -> Option<String> {
        Some("/repo".into())
    }
    fn git_branch(&self, _: &str) -> Option<String> {
        Some("main".into())
    }
    fn git<'a>(&'a self, root: &'a str) -> PanelFuture<'a, Option<PanelGitStatus>> {
        self.git_reads.fetch_add(1, Ordering::SeqCst);
        let pending = self.pending_git.lock().expect("pending git").take();
        Box::pin(async move {
            match pending {
                Some(receiver) => receiver.await.ok(),
                None => Some(PanelGitStatus {
                    root: root.into(),
                    files: vec![],
                }),
            }
        })
    }
    fn children<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> PanelFuture<'a, Result<Vec<PanelChild>, String>> {
        self.children_reads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Ok(vec![PanelChild {
                id: "c1".into(),
                name: "explore".into(),
                status: PanelChildStatus::Running,
                started_at: 0,
                finished_at: None,
                activity: None,
                cost: None,
                card: vec![],
                category: None,
                parked_reason: None,
                host: None,
                turns: None,
                tokens: None,
            }])
        })
    }
    fn goal(&self, _: Option<&str>) -> Option<PanelGoal> {
        None
    }
    fn resolve_memory(&self, _: &str) -> Option<String> {
        Some("identity".into())
    }
    fn memory<'a>(
        &'a self,
        identity: &'a str,
        _: Option<&'a str>,
    ) -> PanelFuture<'a, Option<PanelMemory>> {
        self.memory_reads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Some(PanelMemory {
                identity: identity.into(),
                ..PanelMemory::default()
            })
        })
    }
    fn child_output<'a>(&'a self, _: &'a str, _: &'a str) -> PanelFuture<'a, Vec<PanelRow>> {
        Box::pin(async { vec![] })
    }
}
struct Exec;
impl PanelExec for Exec {
    fn exec<'a>(
        &'a self,
        _: &'a str,
        _: Vec<String>,
        _: &'a str,
        _: u64,
    ) -> PanelFuture<'a, Result<crate::commands::ExecOutput, String>> {
        Box::pin(async {
            Ok(crate::commands::ExecOutput {
                stdout: String::new(),
                code: 0,
            })
        })
    }
}
#[derive(Default)]
struct Timers {
    callbacks: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}
impl PanelTimers for Timers {
    fn set(&self, ms: u64, callback: Box<dyn FnOnce() + Send>) -> Stop {
        assert_eq!(ms, LIVE_REFRESH_MS);
        self.callbacks.lock().expect("callbacks").push(callback);
        Box::new(|| {})
    }
}
fn controller(readers: Arc<Readers>, clock: Arc<AtomicU64>) -> Arc<SidePanelController> {
    create_side_panel_controller(SidePanelRuntimeOptions {
        cwd: "/repo".into(),
        regular_notice_state_dir: None,
        now: Arc::new(move || clock.load(Ordering::SeqCst)),
        readers,
        exec: Some(Arc::new(Exec)),
        timers: Arc::new(Timers::default()),
        defer: Arc::new(|f| f()),
        usage: None,
        track: None,
        debug: Arc::new(|_| {}),
    })
}
#[test]
fn git_and_memory_reads_obey_independent_floors() {
    futures::executor::block_on(async {
        let readers = Arc::new(Readers::default());
        let clock = Arc::new(AtomicU64::new(10_000));
        let controller = controller(readers.clone(), clock.clone());
        {
            let mut s = controller.state.lock().expect("state");
            s.git_root = Some("/repo".into());
            s.memory_identity = Some("identity".into());
        }
        controller.refresh_git(true).await;
        controller.refresh_memory(true).await;
        controller.refresh_git(false).await;
        controller.refresh_memory(false).await;
        assert_eq!(readers.git_reads.load(Ordering::SeqCst), 1);
        assert_eq!(readers.memory_reads.load(Ordering::SeqCst), 1);
        clock.fetch_add(GIT_REFRESH_FLOOR_MS, Ordering::SeqCst);
        controller.refresh_git(false).await;
        controller.refresh_memory(false).await;
        assert_eq!(readers.git_reads.load(Ordering::SeqCst), 2);
        assert_eq!(readers.memory_reads.load(Ordering::SeqCst), 1);
        clock.fetch_add(MEMORY_REFRESH_FLOOR_MS, Ordering::SeqCst);
        controller.refresh_memory(false).await;
        assert_eq!(readers.memory_reads.load(Ordering::SeqCst), 2);
    });
}
#[test]
fn a_read_finishing_after_teardown_cannot_publish() {
    use futures::FutureExt;
    futures::executor::block_on(async {
        let readers = Arc::new(Readers::default());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        *readers.pending_git.lock().expect("pending") = Some(receiver);
        let controller = controller(readers, Arc::new(AtomicU64::new(10_000)));
        controller.state.lock().expect("state").git_root = Some("/repo".into());
        let work = controller.refresh_git(true);
        futures::pin_mut!(work);
        assert!(work.as_mut().now_or_never().is_none());
        controller.teardown();
        sender
            .send(PanelGitStatus {
                root: "old-session".into(),
                files: vec![],
            })
            .expect("receiver alive");
        work.await;
        assert!(controller.state.lock().expect("state").git.is_none());
    });
}
#[test]
fn child_reads_need_session_and_an_enabled_consumer() {
    futures::executor::block_on(async {
        let readers = Arc::new(Readers::default());
        let controller = controller(readers.clone(), Arc::new(AtomicU64::new(0)));
        controller.refresh_children().await;
        assert_eq!(readers.children_reads.load(Ordering::SeqCst), 0);
        {
            let mut s = controller.state.lock().expect("state");
            s.settings = Some(OmoSidePanelSettings::default());
            s.facts.session_id = Some("s1".into());
        }
        controller.refresh_children().await;
        assert_eq!(readers.children_reads.load(Ordering::SeqCst), 1);
        {
            let mut s = controller.state.lock().expect("state");
            let settings = s.settings.as_mut().expect("settings");
            settings.sections.agents = false;
            settings.sections.session = false;
        }
        controller.refresh_children().await;
        assert_eq!(readers.children_reads.load(Ordering::SeqCst), 1);
    });
}
#[test]
fn tool_argument_priority_and_exchange_reset() {
    let payload = serde_json::json!({"toolName":"read","args":{"file_path":"first.rs","path":"second.rs","command":"third"}});
    let call = tool_call_from(&payload, 42).expect("call");
    assert_eq!(call.name, "read");
    assert_eq!(call.detail.as_deref(), Some("first.rs"));
    assert_eq!(call.at, 42);
    assert!(tool_call_from(&Value::Null, 0).is_none());
    assert!(tool_call_from(&serde_json::json!({"name":""}), 0).is_none());
    let controller = controller(Arc::new(Readers::default()), Arc::new(AtomicU64::new(0)));
    controller
        .state
        .lock()
        .expect("state")
        .store
        .record_tool(call);
    controller.input();
    assert!(
        controller
            .state
            .lock()
            .expect("state")
            .store
            .tools
            .is_empty()
    );
}

struct Ui;
impl PanelUi for Ui {
    fn set_widget(&self, _: &str, _: Option<WidgetFactory>, _: bool) {}
    fn notify(&self, _: &str, _: &str) {}
    fn select<'a>(&'a self, _: &'a str, _: Vec<String>) -> PanelFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn on_terminal_input(&self, _: TerminalHandler) -> Option<Stop> {
        None
    }
}

#[tokio::test]
async fn session_start_returns_before_pending_reads_and_tracks_completion() {
    use futures::FutureExt;
    let readers = Arc::new(Readers::default());
    let (release, pending) = tokio::sync::oneshot::channel();
    *readers.pending_git.lock().expect("pending") = Some(pending);
    let (tracked, mut tasks) = tokio::sync::mpsc::unbounded_channel();
    let controller = create_side_panel_controller(SidePanelRuntimeOptions {
        cwd: "/repo".into(),
        regular_notice_state_dir: None,
        now: Arc::new(|| 10_000),
        readers: readers.clone(),
        exec: Some(Arc::new(Exec)),
        timers: Arc::new(Timers::default()),
        defer: Arc::new(|work| work()),
        usage: None,
        track: Some(Arc::new(move |task| {
            tracked.send(task).expect("task receiver");
        })),
        debug: Arc::new(|_| {}),
    });
    let event = PanelEventContext {
        value: serde_json::json!({"mode":"tui", "hasUI":true}),
        ui: Some(Arc::new(Ui)),
    };
    assert!(
        controller
            .session_start(OmoSidePanelSettings::default(), &event)
            .now_or_never()
            .is_some()
    );
    let task = tasks.try_recv().expect("initial reads tracked");
    release
        .send(PanelGitStatus {
            root: "/repo".into(),
            files: vec![],
        })
        .expect("read receiver");
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("read completion")
        .expect("read task");
    assert_eq!(readers.git_reads.load(Ordering::SeqCst), 1);
    assert!(controller.state.lock().expect("state").git.is_some());
    controller.teardown();
}

#[tokio::test(flavor = "current_thread")]
async fn queued_refreshes_cannot_start_reads_after_teardown() {
    let readers = Arc::new(Readers::default());
    let (tracked, mut tasks) = tokio::sync::mpsc::unbounded_channel();
    let controller = create_side_panel_controller(SidePanelRuntimeOptions {
        cwd: "/repo".into(),
        regular_notice_state_dir: None,
        now: Arc::new(|| 10_000),
        readers: readers.clone(),
        exec: Some(Arc::new(Exec)),
        timers: Arc::new(Timers::default()),
        defer: Arc::new(|work| work()),
        usage: None,
        track: Some(Arc::new(move |task| {
            tracked.send(task).expect("task receiver");
        })),
        debug: Arc::new(|_| {}),
    });
    let event = PanelEventContext {
        value: serde_json::json!({"mode":"tui", "hasUI":true}),
        ui: Some(Arc::new(Ui)),
    };
    controller
        .session_start(OmoSidePanelSettings::default(), &event)
        .await;
    controller.refresh(Value::Null);
    controller.tool_end();
    controller.teardown();
    for _ in 0..3 {
        let task = tasks.try_recv().expect("tracked refresh");
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("refresh completion")
            .expect("refresh task");
    }
    assert_eq!(readers.git_reads.load(Ordering::SeqCst), 0);
    assert_eq!(readers.children_reads.load(Ordering::SeqCst), 0);
    assert_eq!(readers.memory_reads.load(Ordering::SeqCst), 0);
}

#[test]
fn regular_notice_marker_is_exclusive_and_fails_closed() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "side-panel-notice-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    ));
    assert!(claim_regular_mode_notice(&dir));
    assert!(!claim_regular_mode_notice(&dir));
    let marker = dir.join(REGULAR_MODE_NOTICE_MARKER);
    let value: Value =
        serde_json::from_slice(&std::fs::read(&marker).expect("marker")).expect("json");
    assert!(value["shownAt"].as_str().is_some());
    assert!(!claim_regular_mode_notice(&marker.join("unwritable")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&marker)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[derive(Default)]
struct NoticeUi {
    notices: AtomicUsize,
}
struct RegularTui;
impl PanelHostTui for RegularTui {
    fn viewport(&self) -> bool {
        false
    }
    fn layout_root(&self) -> Option<Component> {
        None
    }
    fn set_layout_root(&self, _: Option<Component>) {}
    fn request_render(&self, _: bool) {}
    fn columns(&self) -> usize {
        120
    }
    fn open_url(&self) -> Option<UrlHook> {
        None
    }
    fn set_open_url(&self, _: UrlHook) {}
    fn parked_url_hook(&self) -> Option<UrlHook> {
        None
    }
    fn park_url_hook(&self, _: UrlHook) {}
}
impl PanelUi for NoticeUi {
    fn set_widget(&self, key: &str, factory: Option<WidgetFactory>, _: bool) {
        if key == crate::constants::SIDE_PANEL_ANCHOR_WIDGET_KEY
            && let Some(factory) = factory
        {
            factory(Arc::new(RegularTui), None);
        }
    }
    fn notify(&self, _: &str, kind: &str) {
        assert_eq!(kind, "info");
        self.notices.fetch_add(1, Ordering::SeqCst);
    }
    fn select<'a>(&'a self, _: &'a str, _: Vec<String>) -> PanelFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn on_terminal_input(&self, _: TerminalHandler) -> Option<Stop> {
        None
    }
}
#[tokio::test]
async fn regular_mode_notice_is_shared_across_sessions() {
    let dir =
        std::env::temp_dir().join(format!("side-panel-runtime-notice-{}", std::process::id()));
    let ui = Arc::new(NoticeUi::default());
    for _ in 0..2 {
        let mut options = controller(Arc::new(Readers::default()), Arc::new(AtomicU64::new(0)))
            .options
            .clone();
        options.regular_notice_state_dir = Some(dir.clone());
        let (tracked, mut tasks) = tokio::sync::mpsc::unbounded_channel();
        options.track = Some(Arc::new(move |task| {
            tracked.send(task).expect("receiver");
        }));
        let controller = create_side_panel_controller(options);
        controller
            .session_start(
                OmoSidePanelSettings::default(),
                &PanelEventContext {
                    value: serde_json::json!({"mode":"tui","hasUI":true}),
                    ui: Some(ui.clone()),
                },
            )
            .await;
        let task = tasks.try_recv().expect("initial refresh");
        tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .expect("refresh completion")
            .expect("refresh task");
        controller.teardown();
    }
    assert_eq!(ui.notices.load(Ordering::SeqCst), 1);
    std::fs::remove_dir_all(dir).expect("cleanup");
}
