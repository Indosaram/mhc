//! `docs/cleanroom/specs/task.md` "Delta (omo v5.1.52)": D1 (ancestry variables on every run),
//! D3 (remedy line), D4 (first-run fallback away from a provider that cannot authenticate), D7
//! (steers neither lost nor doubled) and D8 (signal an orphaned child only while it is that
//! child). Runs are driven by a fake runner; every wait is on an exact event with a bounded
//! timeout.
use maho_gateway::{Endpoint, Store};
use maho_lsp::hooks::HookChain;
use maho_task::control::{self, Caller};
use maho_task::runner::BoxFuture;
use maho_task::settings::{PARENT_SESSION_ENV, ROOT_SESSION_ENV};
use maho_task::steering::ACT_ON_RECEIVED;
use maho_task::{
    CancelToken, GatewaySteer, ModelChoice, ModelContext, ModelRequest, RunOutcome, RunSpec,
    SpawnRequest, StateDir, SteerChannel, TaskManager, TaskManagerOptions, TaskRecord, TaskRunner,
    TaskSettings, TaskStatus, bundled_agents, choose_model,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

const LIMIT: Duration = Duration::from_secs(10);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// How the fake child behaves on a provider.
#[derive(Clone, Default)]
struct Behaviour {
    /// stderr text written before exiting 1 (no output).
    log: String,
    /// Output written before failing.
    output: Option<String>,
}

/// Writes `echo:<prompt>` and exits 0, unless the run's provider has a [`Behaviour`]. Prompts
/// containing `hold` wait for [`FakeRunner::release`]. Each started spec is reported on `started`.
struct FakeRunner {
    release: tokio::sync::Semaphore,
    failing: Mutex<HashMap<String, Behaviour>>,
    specs: Mutex<Vec<RunSpec>>,
    started: mpsc::UnboundedSender<RunSpec>,
}

impl FakeRunner {
    fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<RunSpec>) {
        let (started, events) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                release: tokio::sync::Semaphore::new(0),
                failing: Mutex::default(),
                specs: Mutex::default(),
                started,
            }),
            events,
        )
    }
    fn fail(&self, provider: &str, behaviour: Behaviour) {
        lock(&self.failing).insert(provider.to_owned(), behaviour);
    }
    fn specs(&self) -> Vec<RunSpec> {
        lock(&self.specs).clone()
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
            lock(&self.specs).push(spec.clone());
            let _ = self.started.send(spec.clone());
            let provider = spec
                .model
                .as_deref()
                .and_then(|m| m.split_once('/'))
                .map(|(p, _)| p.to_owned())
                .unwrap_or_default();
            let behaviour = lock(&self.failing).get(&provider).cloned();
            if let Some(behaviour) = behaviour {
                std::fs::write(&spec.output_path, behaviour.output.unwrap_or_default())
                    .expect("out");
                std::fs::write(&spec.log_path, behaviour.log).expect("log");
                return RunOutcome {
                    exit_code: Some(1),
                    ..RunOutcome::default()
                };
            }
            if spec.prompt.contains("hold") {
                tokio::select! {
                    permit = self.release.acquire() => permit.expect("open").forget(),
                    () = cancel.cancelled() => return RunOutcome { cancelled: true, ..RunOutcome::default() },
                }
            }
            std::fs::write(&spec.output_path, format!("echo:{}", spec.prompt)).expect("out");
            std::fs::write(&spec.log_path, "").expect("log");
            RunOutcome {
                exit_code: Some(0),
                ..RunOutcome::default()
            }
        })
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    manager: Arc<TaskManager>,
    runner: Arc<FakeRunner>,
    started: mpsc::UnboundedReceiver<RunSpec>,
    delivered: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new() -> Self {
        Self::with_concurrency(0)
    }

    fn with_concurrency(concurrency: usize) -> Self {
        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path().to_path_buf();
        let (runner, started) = FakeRunner::new();
        let manager = manager_at(&root, &runner, concurrency);
        let delivered: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink = Arc::clone(&delivered);
        manager.set_on_settled(Some(Arc::new(move |record: &TaskRecord, _: &str| {
            lock(&sink).push(record.id.clone());
        })));
        Self {
            _dir: dir,
            root,
            manager,
            runner,
            started,
            delivered,
        }
    }

    async fn next_start(&mut self) -> RunSpec {
        tokio::time::timeout(LIMIT, self.started.recv())
            .await
            .expect("a run starts in time")
            .expect("runner alive")
    }

    async fn settle(&self, id: &str) -> TaskRecord {
        let record = self.manager.wait(id, Some(LIMIT)).await.expect("record");
        assert!(
            !record.status.is_active(),
            "{id} did not settle: {record:?}"
        );
        record
    }

    fn gateway(&self) -> Store {
        let store = Store::new(self.root.join("gateway"));
        self.manager.set_steer_channel(Some(Arc::new(GatewaySteer {
            store: store.clone(),
            enabled: true,
        }) as Arc<dyn SteerChannel>));
        store
    }

    fn transcript(&self, session_id: &str, lines: &[String]) {
        let dir = self.manager.state().sessions_dir();
        std::fs::create_dir_all(&dir).expect("mkdir");
        let mut text = json!({"type": "session", "id": session_id}).to_string();
        for line in lines {
            text.push('\n');
            text.push_str(line);
        }
        std::fs::write(
            dir.join(format!("2026-10-30T00-00-00-000Z_{session_id}.jsonl")),
            text,
        )
        .expect("transcript");
    }
}

fn manager_at(root: &Path, runner: &Arc<FakeRunner>, concurrency: usize) -> Arc<TaskManager> {
    TaskManager::new(TaskManagerOptions {
        state: StateDir::new(root.join("state")),
        settings: TaskSettings {
            concurrency,
            ..TaskSettings::default()
        },
        runner: Arc::clone(runner) as Arc<dyn TaskRunner>,
        isolation: None,
        observers: Vec::new(),
        hooks: HookChain::default(),
        depth: 0,
    })
}

fn request(cwd: &Path, name: &str, task: &str, choice: ModelChoice) -> SpawnRequest {
    SpawnRequest {
        agent: bundled_agents().remove(0),
        task: task.to_owned(),
        context: None,
        name: Some(name.to_owned()),
        choice,
        cwd: cwd.to_path_buf(),
        background: true,
        isolated: false,
        group: None,
        pane: None,
        quiet: false,
        owner_session: Some("P".into()),
        root_session: Some("P".into()),
    }
}

fn caller() -> Caller {
    Caller {
        session_id: "P".into(),
        root_session: "P".into(),
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

fn delivery_line(id: &str) -> String {
    json!({"type": "custom_message", "id": format!("e-{id}"), "parentId": null, "timestamp": "t",
        "customType": "session_control_delivery", "content": [], "display": true,
        "details": {"delivery_id": id, "source": "session_control"}})
    .to_string()
}

fn assistant_line() -> String {
    json!({"type": "message", "id": "a", "parentId": null, "timestamp": "t",
        "message": {"role": "assistant", "content": [{"type": "text", "text": "ok"}], "stopReason": "stop"}})
    .to_string()
}

fn category_choice(models: &[&str], available: &[&str], parent: Option<&str>) -> ModelChoice {
    let view = json!({"categories": {"c": {"models": models}}});
    let available: Vec<String> = available.iter().map(|m| (*m).to_owned()).collect();
    choose_model(
        &bundled_agents().remove(0),
        &ModelRequest {
            category: Some("c"),
            ..ModelRequest::default()
        },
        &ModelContext {
            view: Some(&view),
            available: &available,
            parent_model: parent,
        },
    )
    .expect("choice")
}

fn signed_out(provider: &str) -> Behaviour {
    Behaviour {
        log: format!("No API key found for {provider}.\n"),
        output: None,
    }
}

// ---------------------------------------------------------------- D1

/// D1 test 2: a resumed task passes the same parent, root and task id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d1_resume_passes_the_same_ancestry() {
    let mut fx = Fixture::new();
    let record = fx
        .manager
        .start(request(&fx.root, "T", "first", ModelChoice::default()))
        .expect("start");
    let first = fx.next_start().await;
    fx.settle(&record.id).await;
    fx.manager
        .resume(&record.id, Some("again"), true)
        .expect("resume");
    let second = fx.next_start().await;
    fx.settle(&record.id).await;
    assert_eq!(first.child_env(), second.child_env());
    assert!(
        first
            .child_env()
            .contains(&(PARENT_SESSION_ENV, "P".to_owned()))
    );
    assert!(
        first
            .child_env()
            .contains(&(ROOT_SESSION_ENV, "P".to_owned()))
    );
    assert_eq!(first.session_id, second.session_id);
}

// ---------------------------------------------------------------- D3 / D4

/// D4 test 1 (and D3 on the dropped attempt's note).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d4_first_run_moves_to_another_provider() {
    let mut fx = Fixture::new();
    fx.runner.fail("a", signed_out("a"));
    let choice = category_choice(&["a/one", "b/two"], &["a/one", "b/two"], None);
    assert_eq!(choice.model.as_deref(), Some("a/one"));
    let record = fx
        .manager
        .start(request(&fx.root, "Job", "do it", choice))
        .expect("start");
    let failed = fx.next_start().await;
    let retried = fx.next_start().await;
    assert_eq!(failed.model.as_deref(), Some("a/one"));
    assert_eq!(retried.model.as_deref(), Some("b/two"));
    assert_eq!(retried.prompt, failed.prompt, "same first prompt");
    assert_ne!(retried.session_id, failed.session_id, "fresh session");
    let settled = fx.settle(&record.id).await;
    assert_eq!(settled.status, TaskStatus::Completed);
    assert_eq!(settled.runs, 1);
    assert_eq!(settled.id, record.id);
    assert_eq!(settled.session_id, retried.session_id);
    assert!(
        settled.model_source.starts_with("category:c"),
        "{}",
        settled.model_source
    );
    assert!(
        settled.model_source.contains("fallback: a"),
        "{}",
        settled.model_source
    );
    assert!(
        settled
            .hook_notes
            .iter()
            .any(|note| note.contains("No API key found for a.")),
        "{:?}",
        settled.hook_notes
    );
    assert_eq!(
        *lock(&fx.delivered),
        [record.id.clone()],
        "one background delivery"
    );
    let envelope = fx.manager.result_text(&record.id).expect("envelope");
    assert!(envelope.contains("No API key found for a."), "{envelope}");
}

/// D4: messages sent while the task waited for a slot ride the retried first run too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d4_retry_keeps_messages_queued_before_the_first_run() {
    let mut fx = Fixture::with_concurrency(1);
    fx.runner.fail("a", signed_out("a"));
    let blocker = fx
        .manager
        .start(request(
            &fx.root,
            "Blocker",
            "hold the slot",
            ModelChoice::default(),
        ))
        .expect("start");
    assert_eq!(fx.next_start().await.prompt, "hold the slot");
    let choice = category_choice(&["a/one", "b/two"], &["a/one", "b/two"], None);
    let record = fx
        .manager
        .start(request(&fx.root, "Job", "do it", choice))
        .expect("start");
    assert_eq!(
        fx.manager.get(&record.id).expect("record").status,
        TaskStatus::Queued
    );
    let sent = control::send(&fx.manager, &caller(), &record.id, "message one", false);
    assert_eq!(sent.kind, "queued", "{sent:?}");
    fx.runner.release.add_permits(1);
    fx.settle(&blocker.id).await;
    let failed = fx.next_start().await;
    let retried = fx.next_start().await;
    assert_eq!(failed.prompt, "do it\n\nmessage one");
    assert_eq!(retried.model.as_deref(), Some("b/two"));
    assert_eq!(retried.prompt, failed.prompt, "queued message kept");
    let settled = fx.settle(&record.id).await;
    assert_eq!(settled.status, TaskStatus::Completed);
    assert_eq!(settled.runs, 1);
    assert!(settled.pending_messages.is_empty());
}

/// D4 test 2 and D3 test 1: no other provider -> failed with the remedy line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d4_same_provider_only_fails_with_the_remedy_line() {
    let mut fx = Fixture::new();
    fx.runner.fail("a", signed_out("a"));
    let choice = category_choice(&["a/one", "a/two"], &["a/one", "a/two"], None);
    let record = fx
        .manager
        .start(request(&fx.root, "Job", "do it", choice))
        .expect("start");
    fx.next_start().await;
    let settled = fx.settle(&record.id).await;
    assert_eq!(settled.status, TaskStatus::Failed);
    assert_eq!(fx.runner.specs().len(), 1, "no retry");
    let error = settled.error.expect("error");
    for needle in [
        "No API key found for a.",
        "`a`",
        "missing",
        "/login a",
        "omo.jsonc",
    ] {
        assert!(error.contains(needle), "{needle}: {error}");
    }
    let envelope = fx.manager.result_text(&record.id).expect("envelope");
    assert!(envelope.contains("/login a"), "{envelope}");
}

/// D4 test 3 and D3 test 2: an explicit model is never re-routed; a rejected credential gets
/// its own remedy line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d4_explicit_model_is_not_rerouted() {
    let mut fx = Fixture::new();
    fx.runner.fail(
        "acme",
        Behaviour {
            log: "HTTP 401 from acme\n".into(),
            output: None,
        },
    );
    let choice = choose_model(
        &bundled_agents().remove(0),
        &ModelRequest {
            explicit_model: Some("acme/m2"),
            ..ModelRequest::default()
        },
        &ModelContext {
            parent_model: Some("b/two"),
            ..ModelContext::default()
        },
    )
    .expect("choice");
    let record = fx
        .manager
        .start(request(&fx.root, "Job", "do it", choice))
        .expect("start");
    fx.next_start().await;
    let settled = fx.settle(&record.id).await;
    assert_eq!(settled.status, TaskStatus::Failed);
    assert_eq!(fx.runner.specs().len(), 1);
    let error = settled.error.expect("error");
    assert!(
        error.contains("rejected") && error.contains("/login acme"),
        "{error}"
    );
}

/// D4 tests 4 and 5, D3 test 3: not a credential failure, or output already written.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d4_no_retry_without_a_clean_credential_failure() {
    for (behaviour, credential) in [
        (
            Behaviour {
                log: "500 internal error\n".into(),
                output: None,
            },
            false,
        ),
        (
            Behaviour {
                log: "401 unauthorized\n".into(),
                output: Some("partial answer".into()),
            },
            true,
        ),
    ] {
        let mut fx = Fixture::new();
        fx.runner.fail("a", behaviour);
        let choice = category_choice(&["a/one", "b/two"], &["a/one", "b/two"], None);
        let record = fx
            .manager
            .start(request(&fx.root, "Job", "do it", choice))
            .expect("start");
        fx.next_start().await;
        let settled = fx.settle(&record.id).await;
        assert_eq!(settled.status, TaskStatus::Failed);
        assert_eq!(fx.runner.specs().len(), 1, "no retry");
        assert_eq!(
            settled
                .error
                .as_deref()
                .is_some_and(|e| e.contains("/login a")),
            credential,
            "{:?}",
            settled.error
        );
    }
}

/// D4 test 6: each provider is dropped once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d4_each_provider_is_dropped_once() {
    let mut fx = Fixture::new();
    fx.runner.fail("a", signed_out("a"));
    fx.runner.fail(
        "b",
        Behaviour {
            log: "OAuth refresh failed for b\n".into(),
            output: None,
        },
    );
    let all = ["a/one", "b/two", "c/three"];
    let choice = category_choice(&all, &all, None);
    let record = fx
        .manager
        .start(request(&fx.root, "Job", "do it", choice))
        .expect("start");
    for _ in 0..3 {
        fx.next_start().await;
    }
    let settled = fx.settle(&record.id).await;
    assert_eq!(settled.status, TaskStatus::Completed);
    assert_eq!(settled.model.as_deref(), Some("c/three"));
    let models: Vec<Option<String>> = fx.runner.specs().into_iter().map(|s| s.model).collect();
    assert_eq!(
        models,
        [
            Some("a/one".into()),
            Some("b/two".into()),
            Some("c/three".into())
        ]
    );
    assert_eq!(settled.dropped_providers, ["a", "b"]);
    assert_eq!(lock(&fx.delivered).len(), 1);
}

// ---------------------------------------------------------------- D7

/// Steers `text` into the held run of `record` and returns the delivery id.
fn steer(fx: &Fixture, store: &Store, record: &TaskRecord, text: &str) -> String {
    let outcome = control::send(&fx.manager, &caller(), &record.id, text, false);
    assert_eq!(outcome.kind, "steered", "{outcome:?}");
    let pending = store.pending(&record.session_id);
    let delivery = pending
        .iter()
        .find(|(_, delivery)| delivery.text == text)
        .expect("inbox file");
    delivery.1.delivery_id.clone()
}

/// D7 test 1: recorded and answered, inbox file left behind -> no follow-up, file removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d7_answered_steer_is_not_repeated() {
    let mut fx = Fixture::new();
    let store = fx.gateway();
    let record = fx
        .manager
        .start(request(&fx.root, "W", "hold on", ModelChoice::default()))
        .expect("start");
    fx.next_start().await;
    store
        .register(&endpoint(&record.session_id))
        .expect("endpoint");
    let id = steer(&fx, &store, &record, "look at b.rs");
    fx.transcript(&record.session_id, &[delivery_line(&id), assistant_line()]);
    fx.runner.release.add_permits(1);
    let settled = fx.settle(&record.id).await;
    assert_eq!((settled.status, settled.runs), (TaskStatus::Completed, 1));
    assert_eq!(fx.runner.specs().len(), 1, "no follow-up run");
    assert!(
        store.pending(&record.session_id).is_empty(),
        "the leftover file is removed"
    );
    assert!(settled.pending_messages.is_empty());
}

/// D7 test 2: recorded, file deleted, no answer -> one follow-up that does not repeat the text.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d7_unanswered_steer_gets_one_follow_up_without_its_text() {
    let mut fx = Fixture::new();
    let store = fx.gateway();
    let record = fx
        .manager
        .start(request(&fx.root, "W", "hold on", ModelChoice::default()))
        .expect("start");
    fx.next_start().await;
    store
        .register(&endpoint(&record.session_id))
        .expect("endpoint");
    let id = steer(&fx, &store, &record, "rename the flag");
    for (path, _) in store.pending(&record.session_id) {
        std::fs::remove_file(path).expect("drain");
    }
    fx.transcript(&record.session_id, &[assistant_line(), delivery_line(&id)]);
    fx.runner.release.add_permits(1);
    let follow_up = fx.next_start().await;
    assert_eq!(follow_up.prompt, ACT_ON_RECEIVED);
    assert!(!follow_up.prompt.contains("rename the flag"));
    let settled = fx.settle(&record.id).await;
    assert_eq!((settled.status, settled.runs), (TaskStatus::Completed, 2));
    assert_eq!(fx.runner.specs().len(), 2);
}

/// D7 test 3: never drained -> file removed, one follow-up carrying the text; unreadable
/// transcript with the file gone -> reported, not resent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d7_undrained_steer_is_delivered_once_and_lost_files_are_reported() {
    let mut fx = Fixture::new();
    let store = fx.gateway();
    let record = fx
        .manager
        .start(request(&fx.root, "W", "hold on", ModelChoice::default()))
        .expect("start");
    fx.next_start().await;
    store
        .register(&endpoint(&record.session_id))
        .expect("endpoint");
    steer(&fx, &store, &record, "add docs");
    // The transcript exists but never recorded the steer.
    fx.transcript(&record.session_id, &[assistant_line()]);
    fx.runner.release.add_permits(1);
    let follow_up = fx.next_start().await;
    assert_eq!(follow_up.prompt, "add docs");
    let settled = fx.settle(&record.id).await;
    assert_eq!(settled.runs, 2);
    assert!(store.pending(&record.session_id).is_empty());
    let texts: usize = fx
        .runner
        .specs()
        .iter()
        .filter(|spec| spec.prompt.contains("add docs"))
        .count();
    assert_eq!(texts, 1);

    // Unreadable transcript (none) and no inbox file left: uncertain, not resent.
    let other = fx
        .manager
        .start(request(&fx.root, "U", "hold again", ModelChoice::default()))
        .expect("start");
    fx.next_start().await;
    store
        .register(&endpoint(&other.session_id))
        .expect("endpoint");
    let id = steer(&fx, &store, &other, "maybe lost");
    for (path, _) in store.pending(&other.session_id) {
        std::fs::remove_file(path).expect("drain");
    }
    fx.runner.release.add_permits(1);
    let settled = fx.settle(&other.id).await;
    assert_eq!(settled.runs, 1, "not resent");
    assert!(
        settled.hook_notes.iter().any(|note| note.contains(&id)),
        "{:?}",
        settled.hook_notes
    );
    let envelope = fx.manager.result_text(&other.id).expect("envelope");
    assert!(envelope.contains(&id), "{envelope}");
}

/// D7 test 4: a pending entry already recorded in the transcript is not sent again after a
/// parent restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d7_restarted_parent_drops_admitted_pending_entries() {
    let mut fx = Fixture::new();
    let record: TaskRecord = serde_json::from_value(json!({
        "id": "R", "agent": "task", "description": "d", "status": "interrupted", "model": null,
        "thinking": null, "model_source": "parent", "session_id": "r-session", "cwd": fx.root,
        "background": true, "pid": null, "exit_code": null, "error": null, "runs": 1,
        "started_at_ms": 1, "finished_at_ms": 2, "owner_session": "P", "root_session": "P",
        "pending_messages": [
            {"id": "d4", "text": "already seen", "created_at": 1},
            {"id": "d5", "text": "still new", "created_at": 2}
        ]
    }))
    .expect("record");
    fx.manager.state().save(&record).expect("save");
    fx.transcript("r-session", &[delivery_line("d4"), assistant_line()]);
    fx.manager
        .resume("R", Some("continue"), true)
        .expect("resume");
    let spec = fx.next_start().await;
    assert!(!spec.prompt.contains("already seen"), "{}", spec.prompt);
    assert_eq!(spec.prompt, "still new\n\ncontinue");
    fx.settle("R").await;
}

// ---------------------------------------------------------------- D8

#[cfg(unix)]
fn foreign_record(id: &str, session: &str, pid: u32) -> TaskRecord {
    let mut record: TaskRecord = serde_json::from_value(json!({
        "id": id, "agent": "task", "description": "d", "status": "running", "model": null,
        "thinking": null, "model_source": "parent", "session_id": session, "cwd": "/",
        "background": true, "pid": pid, "exit_code": null, "error": null, "runs": 1,
        "started_at_ms": 1, "finished_at_ms": null
    }))
    .expect("record");
    record.pid_started_at_ms = Some(maho_task::record::now_ms());
    record
}

/// D8 test 1: a live pid whose command line lacks the session id is not signalled.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d8_reused_pid_is_not_signalled() {
    let fx = Fixture::new();
    let mut bystander = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("sleep");
    fx.manager
        .state()
        .save(&foreign_record("Reused", "reused-session", bystander.id()))
        .expect("save");
    let outcome = control::cancel(&fx.manager, &caller(), "Reused", None).await;
    assert_eq!(outcome.kind, "cancelled", "{outcome:?}");
    assert_eq!(outcome.details["signalled"], json!(false));
    assert_eq!(
        fx.manager.get("Reused").expect("record").status,
        TaskStatus::Cancelled
    );
    assert!(
        bystander.try_wait().expect("status").is_none(),
        "nothing was signalled"
    );

    // The manager's own cancel path applies the same check.
    fx.manager
        .state()
        .save(&foreign_record("Reused2", "other-session", bystander.id()))
        .expect("save");
    fx.manager.cancel("Reused2");
    assert!(
        bystander.try_wait().expect("status").is_none(),
        "nothing was signalled"
    );
    bystander.kill().expect("kill");
    let _ = bystander.wait();
}

/// D8 test 2: the real child (its command line carries `--session-id <id>`) is signalled.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d8_real_child_is_signalled() {
    let fx = Fixture::new();
    let mut child = tokio::process::Command::new("sh")
        .args([
            "-c",
            "while :; do sleep 1; done",
            "--session-id",
            "child-session",
        ])
        .spawn()
        .expect("sh");
    let pid = child.id().expect("pid");
    fx.manager
        .state()
        .save(&foreign_record("Real", "child-session", pid))
        .expect("save");
    let outcome = control::cancel(&fx.manager, &caller(), "Real", None).await;
    assert_eq!(outcome.kind, "cancelled", "{outcome:?}");
    assert_eq!(outcome.details["signalled"], json!(true));
    let status = tokio::time::timeout(LIMIT, child.wait())
        .await
        .expect("exits in time")
        .expect("status");
    assert!(!status.success());
}
