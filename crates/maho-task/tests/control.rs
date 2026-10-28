//! Acceptance tests 1-6, 8 and 9 of `docs/cleanroom/specs/task-control.md` §6 (test 7 and the
//! tasklist tests live in `maho-team`). Runs are driven by a fake runner whose `hold` prompts wait
//! for an explicit release; every wait is on an exact event with a bounded timeout.
use maho_ext_api::{ExtensionMode, NotificationType};
use maho_gateway::{Endpoint, Mode, Store};
use maho_lsp::hooks::HookChain;
use maho_task::control::{self, Caller, DelegatedEndpoint, DelegatedInbox, KillUi};
use maho_task::runner::BoxFuture;
use maho_task::{
    CancelToken, GatewaySteer, ModelChoice, RunOutcome, RunSpec, SpawnRequest, StateDir,
    SteerChannel, TaskManager, TaskManagerOptions, TaskRecord, TaskRunner, TaskSettings,
    TaskStatus, bundled_agents,
};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

const LIMIT: Duration = Duration::from_secs(10);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Writes `echo:<prompt>`; prompts containing `hold` wait for [`FakeRunner::release`] (or a
/// cancel). Each started prompt is reported on `started`.
struct FakeRunner {
    release: tokio::sync::Semaphore,
    prompts: Mutex<Vec<String>>,
    started: mpsc::UnboundedSender<String>,
}

impl FakeRunner {
    fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<String>) {
        let (started, events) = mpsc::unbounded_channel();
        let runner = Arc::new(Self {
            release: tokio::sync::Semaphore::new(0),
            prompts: Mutex::default(),
            started,
        });
        (runner, events)
    }
    fn release(&self) {
        self.release.add_permits(1);
    }
    fn prompts(&self) -> Vec<String> {
        lock(&self.prompts).clone()
    }
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
            let _ = self.started.send(spec.prompt.clone());
            let finish = || {
                std::fs::write(&spec.output_path, format!("echo:{}", spec.prompt)).expect("output");
                RunOutcome {
                    exit_code: Some(0),
                    ..RunOutcome::default()
                }
            };
            if spec.prompt.contains("hold") {
                tokio::select! {
                    permit = self.release.acquire() => {
                        permit.expect("semaphore open").forget();
                        finish()
                    }
                    () = cancel.cancelled() => RunOutcome { cancelled: true, ..RunOutcome::default() },
                }
            } else {
                finish()
            }
        })
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    manager: Arc<TaskManager>,
    runner: Arc<FakeRunner>,
    started: mpsc::UnboundedReceiver<String>,
}

impl Fixture {
    fn new(concurrency: usize) -> Self {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path().to_path_buf();
        let (runner, started) = FakeRunner::new();
        let manager = TaskManager::new(TaskManagerOptions {
            state: StateDir::new(root.join("state")),
            settings: TaskSettings {
                concurrency,
                ..TaskSettings::default()
            },
            runner: Arc::clone(&runner) as Arc<dyn TaskRunner>,
            isolation: None,
            observers: Vec::new(),
            hooks: HookChain::default(),
            depth: 0,
        });
        Self {
            _dir: dir,
            root,
            manager,
            runner,
            started,
        }
    }

    fn start(&self, name: &str, task: &str, owner: &str) -> TaskRecord {
        self.manager
            .start(request(&self.root, name, task, owner))
            .expect("start")
    }

    /// Waits for the next run to start and returns its prompt.
    async fn next_start(&mut self) -> String {
        tokio::time::timeout(LIMIT, self.started.recv())
            .await
            .expect("a run starts in time")
            .expect("runner alive")
    }

    async fn settle(&self, id: &str) -> TaskRecord {
        let record = self.manager.wait(id, Some(LIMIT)).await.expect("record");
        assert!(!record.status.is_active(), "{id} did not settle: {record:?}");
        record
    }

    fn gateway(&self, enabled: bool) -> Store {
        let store = Store::new(self.root.join("gateway"));
        self.manager.set_steer_channel(Some(Arc::new(GatewaySteer {
            store: store.clone(),
            enabled,
        }) as Arc<dyn SteerChannel>));
        store
    }
}

fn request(cwd: &Path, name: &str, task: &str, owner: &str) -> SpawnRequest {
    SpawnRequest {
        agent: bundled_agents().remove(0),
        task: task.to_owned(),
        context: None,
        name: Some(name.to_owned()),
        choice: ModelChoice::default(),
        cwd: cwd.to_path_buf(),
        background: true,
        isolated: false,
        group: None,
        pane: None,
        quiet: false,
        owner_session: Some(owner.to_owned()),
        root_session: Some(owner.to_owned()),
    }
}

fn caller(session: &str) -> Caller {
    Caller {
        session_id: session.to_owned(),
        root_session: session.to_owned(),
    }
}

fn endpoint(session_id: &str) -> Endpoint {
    Endpoint {
        version: 1,
        session_id: session_id.to_owned(),
        name: None,
        cwd: "/".into(),
        pid: std::process::id(),
        started_at: maho_gateway::store::now(),
    }
}

/// Acceptance test 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_a_queued_task_queues_in_order_for_the_first_run() {
    let mut fx = Fixture::new(1);
    let blocker = fx.start("Blocker", "hold the slot", "s1");
    assert_eq!(fx.next_start().await, "hold the slot");
    let queued = fx.start("Second", "second task", "s1");
    assert_eq!(fx.manager.get(&queued.id).expect("record").status, TaskStatus::Queued);

    let first = control::send(&fx.manager, &caller("s1"), &queued.id, "message one", false);
    let second = control::send(&fx.manager, &caller("s1"), "Second", "message two", false);
    assert_eq!((first.kind, second.kind), ("queued", "queued"), "{first:?} {second:?}");
    assert_eq!(first.details["position"], json!(1));
    assert_eq!(second.details["position"], json!(2));

    fx.runner.release();
    fx.settle(&blocker.id).await;
    assert_eq!(fx.next_start().await, "second task\n\nmessage one\n\nmessage two");
    let record = fx.settle(&queued.id).await;
    assert_eq!(record.status, TaskStatus::Completed);
    assert_eq!(record.runs, 1, "the messages ride the first run");
    assert!(record.pending_messages.is_empty());
}

/// Acceptance test 2: steering through a live gateway endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_a_running_child_steers_through_a_live_endpoint() {
    let mut fx = Fixture::new(0);
    let store = fx.gateway(true);
    let record = fx.start("Worker", "hold on", "parent-1");
    fx.next_start().await;
    store.register(&endpoint(&record.session_id)).expect("endpoint");

    let outcome = control::send(&fx.manager, &caller("parent-1"), &record.id, "look at b.rs", false);
    assert_eq!(outcome.kind, "steered", "{outcome:?}");
    let pending = store.pending(&record.session_id);
    assert_eq!(pending.len(), 1);
    let delivery = &pending[0].1;
    assert_eq!(delivery.deliver_as, Mode::Steer);
    assert_eq!(delivery.hop, 1);
    assert_eq!(delivery.text, "look at b.rs");
    assert_eq!(
        serde_json::to_value(&delivery.sender).expect("sender"),
        json!({"kind": "agent", "session_id": "parent-1"})
    );

    // The child never drained it: when the run settles the steer moves to one follow-up run.
    fx.runner.release();
    assert_eq!(fx.next_start().await, "look at b.rs");
    let settled = fx.settle(&record.id).await;
    assert_eq!(settled.runs, 2);
    assert!(store.pending(&record.session_id).is_empty(), "the undrained steer file is removed");
    let deliveries = fx
        .runner
        .prompts()
        .iter()
        .filter(|prompt| prompt.contains("look at b.rs"))
        .count();
    assert_eq!(deliveries, 1, "delivered exactly once: {:?}", fx.runner.prompts());
}

/// Acceptance test 2: no endpoint, or the gateway disabled, queues a follow-up run.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_without_a_live_channel_runs_as_a_follow_up() {
    for enabled in [true, false] {
        let mut fx = Fixture::new(0);
        let store = fx.gateway(enabled);
        let record = fx.start("Worker", "hold on", "s1");
        fx.next_start().await;
        if !enabled {
            // Even a live endpoint is not used while the gateway is off.
            store.register(&endpoint(&record.session_id)).expect("endpoint");
        }
        let outcome = control::send(&fx.manager, &caller("s1"), &record.id, "then add docs", false);
        assert_eq!(outcome.kind, "queued", "enabled={enabled}: {outcome:?}");
        assert_eq!(outcome.details["position"], json!(1));
        assert!(store.pending(&record.session_id).is_empty());
        fx.runner.release();
        assert_eq!(fx.next_start().await, "then add docs");
        let settled = fx.settle(&record.id).await;
        assert_eq!((settled.status, settled.runs), (TaskStatus::Completed, 2));
        assert_eq!(fx.manager.state().read_output(&record.id), "echo:then add docs");
    }
}

/// Acceptance test 2: a delegated run (with `MHC_TASK_ID`) keeps an endpoint while it runs and
/// drains its inbox; a plain `-p` run does not receive.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delegated_runs_register_an_endpoint_and_plain_print_runs_do_not() {
    assert!(control::delegated_receiver(ExtensionMode::Print, Some("Worker")));
    assert!(control::delegated_receiver(ExtensionMode::Json, Some("Worker")));
    assert!(!control::delegated_receiver(ExtensionMode::Print, None));
    assert!(!control::delegated_receiver(ExtensionMode::Print, Some("  ")));
    assert!(!maho_gateway::receives_in(ExtensionMode::Print));

    let dir = tempfile::tempdir().expect("tmp");
    let store = Store::new(dir.path().join("gateway"));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let deliver: control::DeliverFn = Arc::new(move |message, options| {
        tx.send((message, options)).map_err(|error| error.to_string())
    });
    let inbox = DelegatedInbox::start(
        store.clone(),
        "child-session",
        "/work",
        Duration::from_millis(50),
        deliver,
    )
    .expect("inbox");
    assert_eq!(store.live().len(), 1);
    assert_eq!(store.live()[0].session_id, "child-session");

    let steer = GatewaySteer {
        store: store.clone(),
        enabled: true,
    };
    let path = steer
        .steer("child-session", "parent", "hello child")
        .expect("live channel");
    let (message, options) = tokio::time::timeout(LIMIT, rx.recv())
        .await
        .expect("delivered in time")
        .expect("channel open");
    assert_eq!(message.custom_type, maho_gateway::DELIVERY_TYPE);
    assert_eq!(options.deliver_as, Some(maho_ext_api::DeliverAs::Steer));
    assert!(path.starts_with(store.inbox_dir("child-session")));

    drop(inbox);
    assert!(store.live().is_empty(), "the record is removed at exit");
    assert!(steer.steer("child-session", "parent", "late").is_none());

    let endpoint = DelegatedEndpoint::register(store.clone(), "other", "/").expect("endpoint");
    assert_eq!(store.live().len(), 1);
    endpoint.close();
    assert!(store.live().is_empty());
}

/// Acceptance test 3.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_a_completed_task_revives_it_in_the_same_session() {
    let mut fx = Fixture::new(0);
    let record = fx.start("Done", "finish quickly", "s1");
    fx.next_start().await;
    let settled = fx.settle(&record.id).await;
    assert_eq!((settled.status, settled.runs), (TaskStatus::Completed, 1));

    let outcome = control::send(&fx.manager, &caller("s1"), "Done", "one more change", false);
    assert_eq!(outcome.kind, "revived", "{outcome:?}");
    assert_eq!(outcome.details["run"], json!(2));
    assert_eq!(fx.next_start().await, "one more change");
    let revived = fx.settle(&record.id).await;
    assert_eq!(revived.runs, 2);
    assert_eq!(revived.session_id, settled.session_id);
    assert_eq!(revived.status, TaskStatus::Completed);
}

/// Acceptance test 4.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_to_a_cancelled_task_is_not_continuable() {
    let mut fx = Fixture::new(0);
    let record = fx.start("Stopped", "hold forever", "s1");
    fx.next_start().await;
    let cancelled = control::cancel(&fx.manager, &caller("s1"), &record.id, None).await;
    assert_eq!(cancelled.kind, "cancelled");
    let before = std::fs::read(fx.manager.state().record_path(&record.id)).expect("record");

    let outcome = control::send(&fx.manager, &caller("s1"), &record.id, "keep going", false);
    assert_eq!(outcome.kind, "not_continuable", "{outcome:?}");
    assert!(outcome.text.contains("cancelled"));
    let after = std::fs::read(fx.manager.state().record_path(&record.id)).expect("record");
    assert_eq!(before, after, "the record is unchanged");
    assert_eq!(fx.runner.prompts().len(), 1);
}

/// Acceptance test 5.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn other_sessions_need_all_sessions() {
    let mut fx = Fixture::new(0);
    let record = fx.start("Owned", "finish", "owner-session");
    fx.next_start().await;
    fx.settle(&record.id).await;

    let denied = control::send(&fx.manager, &caller("intruder"), &record.id, "hi", false);
    assert_eq!(denied.kind, "scope_denied", "{denied:?}");
    assert!(denied.text.contains("owner-session"));
    assert!(denied.is_error());

    let allowed = control::send(&fx.manager, &caller("intruder"), &record.id, "hi", true);
    assert_eq!(allowed.kind, "revived", "{allowed:?}");
    fx.next_start().await;
    fx.settle(&record.id).await;

    // The owner's root session may message it too.
    let mut child_owned = request(&fx.root, "Nested", "finish", "child-session");
    child_owned.root_session = Some("root-session".into());
    let nested = fx.manager.start(child_owned).expect("start");
    fx.next_start().await;
    fx.settle(&nested.id).await;
    let from_root = control::send(&fx.manager, &caller("root-session"), &nested.id, "more", false);
    assert_eq!(from_root.kind, "revived", "{from_root:?}");
    fx.next_start().await;
    fx.settle(&nested.id).await;
}

/// Acceptance test 6.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_messages_and_unknown_targets() {
    let mut fx = Fixture::new(0);
    let record = fx.start("Active", "hold", "s1");
    fx.next_start().await;

    let big = "x".repeat(33 * 1024);
    let outcome = control::send(&fx.manager, &caller("s1"), &record.id, &big, false);
    assert_eq!(outcome.kind, "invalid_arguments");
    assert!(outcome.text.contains("file"), "{}", outcome.text);
    assert_eq!(
        control::send(&fx.manager, &caller("s1"), &record.id, " \n\t", false).kind,
        "invalid_arguments"
    );
    let at_limit = "y".repeat(control::MAX_MESSAGE_BYTES);
    assert_ne!(
        control::send(&fx.manager, &caller("s1"), &record.id, &at_limit, false).kind,
        "invalid_arguments"
    );

    let missing = control::send(&fx.manager, &caller("s1"), "NoSuchTask", "hi", false);
    assert_eq!(missing.kind, "not_found");
    assert_eq!(missing.details["candidates"], json!([record.id]));

    let cancelled = control::cancel(&fx.manager, &caller("s1"), &record.id, None).await;
    assert_eq!(cancelled.kind, "cancelled");
}

/// Acceptance test 8: cancelling a running task, then the `noop` cases.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_running_then_noop_cases() {
    let mut fx = Fixture::new(0);
    let record = fx.start("Long", "hold the line", "s1");
    fx.next_start().await;
    for text in ["first", "second"] {
        let outcome = control::send(&fx.manager, &caller("s1"), &record.id, text, false);
        assert_eq!(outcome.kind, "queued");
    }

    let outcome = control::cancel(&fx.manager, &caller("s1"), "Long", Some("no longer needed")).await;
    assert_eq!(outcome.kind, "cancelled", "{outcome:?}");
    assert_eq!(outcome.details["dropped_messages"], json!(2));
    assert_eq!(outcome.details["previous"], json!("running"));
    assert!(outcome.text.contains("running") && outcome.text.contains("cancelled"));
    assert!(outcome.text.contains('2'), "{}", outcome.text);
    let stored = fx.manager.get(&record.id).expect("record");
    assert_eq!(stored.status, TaskStatus::Cancelled);
    assert_eq!(stored.error.as_deref(), Some("no longer needed"));
    assert!(stored.pending_messages.is_empty());
    assert_eq!(fx.runner.prompts(), ["hold the line"], "no follow-up ran");

    let again = control::cancel(&fx.manager, &caller("s1"), &record.id, None).await;
    assert_eq!(again.kind, "noop");
    assert!(again.text.contains("already cancelled"));

    let done = fx.start("Quick", "quick", "s1");
    fx.next_start().await;
    fx.settle(&done.id).await;
    let noop = control::cancel(&fx.manager, &caller("s1"), &done.id, None).await;
    assert_eq!(noop.kind, "noop");
    assert!(noop.text.contains("completed"));
    assert_eq!(fx.manager.get(&done.id).expect("record").status, TaskStatus::Completed);

    let missing = control::cancel(&fx.manager, &caller("s1"), "Nope", None).await;
    assert_eq!(missing.kind, "not_found");
}

/// Acceptance test 8: a queued task is cancelled before it ever starts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_queued_task_never_starts() {
    let mut fx = Fixture::new(1);
    let blocker = fx.start("Blocker", "hold", "s1");
    fx.next_start().await;
    let queued = fx.start("Waiting", "never runs", "s1");
    let outcome = control::cancel(&fx.manager, &caller("s1"), &queued.id, None).await;
    assert_eq!(outcome.kind, "cancelled");
    assert_eq!(outcome.details["previous"], json!("queued"));
    assert_eq!(fx.manager.get(&queued.id).expect("record").status, TaskStatus::Cancelled);
    fx.runner.release();
    fx.settle(&blocker.id).await;
    assert!(!fx.runner.prompts().iter().any(|p| p == "never runs"));
}

fn stored_record(id: &str, status: &str, pid: Option<u32>) -> TaskRecord {
    serde_json::from_value(json!({
        "id": id, "agent": "task", "description": "d", "status": status, "model": null,
        "thinking": null, "model_source": "parent", "session_id": format!("{id}-session"),
        "cwd": "/", "background": true, "pid": pid, "exit_code": null, "error": null,
        "runs": 1, "started_at_ms": 1, "finished_at_ms": null,
        "pending_messages": [{"id": "m1", "text": "left over", "created_at": 1}]
    }))
    .expect("record")
}

/// Acceptance test 8: an interrupted task becomes cancelled without signalling anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_interrupted_signals_nothing() {
    let fx = Fixture::new(0);
    #[cfg(unix)]
    let mut bystander = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    #[cfg(unix)]
    let pid = Some(bystander.id());
    #[cfg(not(unix))]
    let pid = None;
    fx.manager
        .state()
        .save(&stored_record("Orphan", "interrupted", pid))
        .expect("save");

    let outcome = control::cancel(&fx.manager, &caller("s1"), "Orphan", None).await;
    assert_eq!(outcome.kind, "cancelled", "{outcome:?}");
    assert_eq!(outcome.details["signalled"], json!(false));
    assert_eq!(outcome.details["dropped_messages"], json!(1));
    let stored = fx.manager.get("Orphan").expect("record");
    assert_eq!(stored.status, TaskStatus::Cancelled);
    assert!(stored.pending_messages.is_empty());
    #[cfg(unix)]
    {
        assert!(bystander.try_wait().expect("status").is_none(), "nothing was signalled");
        bystander.kill().expect("kill");
        let _ = bystander.wait();
    }
}

/// Spec §2: a task running in another process is stopped by signalling its pid.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_foreign_running_task_signals_its_owner() {
    let fx = Fixture::new(0);
    let mut child = tokio::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    let pid = child.id().expect("pid");
    fx.manager
        .state()
        .save(&stored_record("Foreign", "running", Some(pid)))
        .expect("save");
    let outcome = control::cancel(&fx.manager, &caller("s1"), "Foreign", Some("stop")).await;
    assert_eq!(outcome.kind, "cancelled", "{outcome:?}");
    assert_eq!(outcome.details["signalled"], json!(true));
    let status = tokio::time::timeout(LIMIT, child.wait())
        .await
        .expect("exits in time")
        .expect("status");
    assert!(!status.success());
    let stored = fx.manager.get("Foreign").expect("record");
    assert_eq!(stored.status, TaskStatus::Cancelled);
    assert_eq!(stored.error.as_deref(), Some("stop"));
}

/// A scripted `/task-kill` UI.
#[derive(Default)]
struct ScriptUi {
    interactive: bool,
    choose: Option<usize>,
    confirm: bool,
    offered: Mutex<Vec<Vec<String>>>,
    confirms: Mutex<Vec<String>>,
    notices: Mutex<Vec<(String, NotificationType)>>,
}

impl KillUi for ScriptUi {
    fn has_ui(&self) -> bool {
        self.interactive
    }
    fn select<'a>(&'a self, _title: &'a str, options: &'a [String]) -> BoxFuture<'a, Option<String>> {
        lock(&self.offered).push(options.to_vec());
        let chosen = self.choose.and_then(|index| options.get(index).cloned());
        Box::pin(async move { chosen })
    }
    fn confirm<'a>(&'a self, _title: &'a str, message: &'a str) -> BoxFuture<'a, bool> {
        lock(&self.confirms).push(message.to_owned());
        let answer = self.confirm;
        Box::pin(async move { answer })
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        lock(&self.notices).push((message.to_owned(), kind));
    }
}

fn statuses(manager: &TaskManager) -> Vec<(String, TaskStatus)> {
    manager
        .list()
        .into_iter()
        .map(|record| (record.id, record.status))
        .collect()
}

/// Acceptance test 9.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_kill_dialogs() {
    let mut fx = Fixture::new(1);
    let me = caller("s1");

    let ui = ScriptUi {
        interactive: true,
        ..ScriptUi::default()
    };
    assert_eq!(control::task_kill(&fx.manager, &me, &ui).await, None);
    assert_eq!(lock(&ui.notices).len(), 1);
    assert_eq!(lock(&ui.notices)[0].1, NotificationType::Info);
    assert!(lock(&ui.offered).is_empty(), "no dialog without candidates");

    let running = fx.start("Job", "hold", "s1");
    fx.next_start().await;
    let queued = fx.start("Job", "later", "s1");
    assert_eq!(queued.id, "Job-2");
    let mut orphan = stored_record("Orphan", "interrupted", None);
    orphan.started_at_ms = u64::MAX;
    fx.manager.state().save(&orphan).expect("save");
    fx.manager
        .state()
        .save(&stored_record("Finished", "completed", None))
        .expect("save");
    fx.manager
        .state()
        .save(&stored_record("Gone", "cancelled", None))
        .expect("save");
    let mut foreign = stored_record("Theirs", "interrupted", None);
    foreign.owner_session = Some("someone-else".into());
    fx.manager.state().save(&foreign).expect("save");
    let before = statuses(&fx.manager);

    let expected_labels = vec![
        "Job running".to_owned(),
        "Job (Job-2) queued".to_owned(),
        "Orphan interrupted".to_owned(),
    ];
    let dismiss = ScriptUi {
        interactive: true,
        choose: None,
        ..ScriptUi::default()
    };
    assert_eq!(control::task_kill(&fx.manager, &me, &dismiss).await, None);
    let mut offered = lock(&dismiss.offered)[0].clone();
    offered.sort();
    let mut sorted = expected_labels.clone();
    sorted.sort();
    assert_eq!(offered, sorted);
    assert!(lock(&dismiss.confirms).is_empty());
    assert_eq!(statuses(&fx.manager), before);

    let decline = ScriptUi {
        interactive: true,
        choose: Some(0),
        confirm: false,
        ..ScriptUi::default()
    };
    assert_eq!(control::task_kill(&fx.manager, &me, &decline).await, None);
    assert_eq!(lock(&decline.confirms).len(), 1);
    assert_eq!(statuses(&fx.manager), before);

    let options = lock(&dismiss.offered)[0].clone();
    let index = options
        .iter()
        .position(|label| label == "Orphan interrupted")
        .expect("orphan offered");
    let accept = ScriptUi {
        interactive: true,
        choose: Some(index),
        confirm: true,
        ..ScriptUi::default()
    };
    assert_eq!(
        control::task_kill(&fx.manager, &me, &accept).await.as_deref(),
        Some("Orphan")
    );
    assert!(lock(&accept.confirms)[0].contains("Orphan"));
    let notices = lock(&accept.notices).clone();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].0.contains("Orphan") && notices[0].1 == NotificationType::Info);
    let orphan = fx.manager.get("Orphan").expect("record");
    assert_eq!(orphan.status, TaskStatus::Cancelled);
    assert_eq!(orphan.error.as_deref(), Some(control::KILL_REASON));
    for (id, status) in statuses(&fx.manager) {
        if id != "Orphan" {
            let previous = before.iter().find(|(b, _)| *b == id).map(|(_, s)| *s);
            assert_eq!(Some(status), previous, "{id} unchanged");
        }
    }

    let headless = ScriptUi::default();
    assert_eq!(control::task_kill(&fx.manager, &me, &headless).await, None);
    assert!(lock(&headless.offered).is_empty());
    let text = lock(&headless.notices)[0].0.clone();
    assert!(text.contains("task_cancel") && text.contains("Job-2"), "{text}");

    control::cancel(&fx.manager, &me, &running.id, None).await;
    control::cancel(&fx.manager, &me, &queued.id, None).await;
}

/// `task_send` details always carry the outcome kind.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outcomes_report_their_kind() {
    let fx = Fixture::new(0);
    let outcome = control::send(&fx.manager, &caller("s1"), "missing", "hi", false);
    assert_eq!(outcome.details["kind"], Value::String("not_found".into()));
}
