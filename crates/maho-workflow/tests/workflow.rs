//! Acceptance tests of docs/cleanroom/specs/workflow-dag.md §7, written from the spec. A fake
//! executor stands in for child tasks: the test decides when and how each child ends, and every
//! wait is on an exact event (child start, scheduler event, notification) with a bounded timeout.
use maho_workflow::definition::{WorkflowDefinition, WorkflowNode};
use maho_workflow::executor::{BoxFuture, ChildOutcome, ChildProbe, NodeExecutor, NodeRequest, SendOutcome, StartError};
use maho_workflow::scheduler::{RunEvent, SchedulerOptions, WorkflowScheduler};
use maho_workflow::skills::DirSkillResolver;
use maho_workflow::state::{NodeState, RunStatus};
use maho_workflow::{Caller, DagSettings, WorkflowStore, dag, tool};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

const LIMIT: Duration = Duration::from_secs(10);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Debug)]
struct Started {
    node: String,
    task: String,
    attempt: u32,
    prompt: String,
}

/// Children whose outcome the test sets. Survives a scheduler (children outlive their owner).
struct FakeExecutor {
    next: AtomicUsize,
    children: Mutex<HashMap<String, watch::Sender<Option<ChildOutcome>>>>,
    starts: mpsc::UnboundedSender<Started>,
    cancelled: Mutex<Vec<String>>,
    sent: Mutex<Vec<(String, String)>>,
}

impl FakeExecutor {
    fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<Started>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                next: AtomicUsize::new(1),
                children: Mutex::default(),
                starts: tx,
                cancelled: Mutex::default(),
                sent: Mutex::default(),
            }),
            rx,
        )
    }

    fn finish(&self, task: &str, outcome: ChildOutcome) {
        let children = lock(&self.children);
        let sender = children.get(task).expect("known child");
        sender.send_replace(Some(outcome));
    }

    fn complete(&self, task: &str) {
        self.finish(task, ChildOutcome::Completed { output: format!("result of {task}") });
    }
}

impl NodeExecutor for FakeExecutor {
    fn start(&self, request: &NodeRequest) -> Result<String, StartError> {
        let task = format!("t{}", self.next.fetch_add(1, Ordering::SeqCst));
        lock(&self.children).insert(task.clone(), watch::channel(None).0);
        let _ = self.starts.send(Started {
            node: request.node_id.clone(),
            task: task.clone(),
            attempt: request.attempt,
            prompt: request.prompt.clone(),
        });
        Ok(task)
    }

    fn wait(&self, task_id: &str) -> BoxFuture<'static, ChildOutcome> {
        let receiver = lock(&self.children).get(task_id).map(watch::Sender::subscribe);
        Box::pin(async move {
            let Some(mut receiver) = receiver else {
                return ChildOutcome::Failed { message: "unknown child".into(), infrastructure: true };
            };
            match receiver.wait_for(Option::is_some).await {
                Ok(outcome) => outcome.clone().unwrap_or(ChildOutcome::Cancelled),
                Err(_) => ChildOutcome::Failed { message: "child channel closed".into(), infrastructure: true },
            }
        })
    }

    fn cancel(&self, task_id: &str) {
        lock(&self.cancelled).push(task_id.to_owned());
        if let Some(sender) = lock(&self.children).get(task_id) {
            sender.send_if_modified(|outcome| {
                if outcome.is_none() {
                    *outcome = Some(ChildOutcome::Cancelled);
                    true
                } else {
                    false
                }
            });
        }
    }

    fn send(&self, task_id: &str, message: &str) -> SendOutcome {
        lock(&self.sent).push((task_id.to_owned(), message.to_owned()));
        match lock(&self.children).get(task_id).map(|s| s.borrow().is_none()) {
            Some(true) => SendOutcome::Steered,
            Some(false) => SendOutcome::FollowUp,
            None => SendOutcome::NotContinuable,
        }
    }

    fn probe(&self, task_id: &str) -> ChildProbe {
        match lock(&self.children).get(task_id).map(|s| s.borrow().clone()) {
            None => ChildProbe::Missing,
            Some(None) => ChildProbe::Running,
            Some(Some(outcome)) => ChildProbe::Settled(outcome),
        }
    }
}

struct Harness {
    dir: tempfile::TempDir,
    exec: Arc<FakeExecutor>,
    starts: mpsc::UnboundedReceiver<Started>,
    scheduler: Arc<WorkflowScheduler>,
    events: tokio::sync::broadcast::Receiver<RunEvent>,
    caller: Caller,
}

fn scheduler_at(root: &Path, exec: &Arc<FakeExecutor>, settings: DagSettings) -> Arc<WorkflowScheduler> {
    WorkflowScheduler::new(SchedulerOptions {
        store: WorkflowStore::in_state_dir(root),
        settings,
        executor: Arc::clone(exec) as Arc<dyn NodeExecutor>,
        skills: Arc::new(DirSkillResolver { roots: vec![root.join("skills")] }),
        retry_base: Duration::from_secs(1),
    })
}

fn harness_with(settings: DagSettings) -> Harness {
    let dir = tempfile::tempdir().expect("tmp");
    let (exec, starts) = FakeExecutor::new();
    let scheduler = scheduler_at(dir.path(), &exec, settings);
    let events = scheduler.subscribe();
    Harness { dir, exec, starts, scheduler, events, caller: Caller::new("parent-1") }
}

fn harness() -> Harness {
    harness_with(DagSettings::default())
}

impl Harness {
    async fn next_start(&mut self) -> Started {
        tokio::time::timeout(LIMIT, self.starts.recv())
            .await
            .expect("a child start within the limit")
            .expect("start channel open")
    }

    fn no_pending_start(&mut self) {
        assert!(self.starts.try_recv().is_err(), "no further child should have started");
    }

    async fn event(&mut self, matches: impl Fn(&RunEvent) -> bool) -> RunEvent {
        tokio::time::timeout(LIMIT, async {
            loop {
                match self.events.recv().await {
                    Ok(event) if matches(&event) => return event,
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(error) => panic!("event channel: {error}"),
                }
            }
        })
        .await
        .expect("the expected event within the limit")
    }

    async fn settled(&mut self, run_id: &str) -> RunStatus {
        let run_id = run_id.to_owned();
        match self.event(|e| matches!(e, RunEvent::RunSettled { run_id: r, .. } if *r == run_id)).await {
            RunEvent::RunSettled { status, .. } => status,
            _ => unreachable!(),
        }
    }

    async fn node_settled(&mut self, node: &str) -> NodeState {
        let node = node.to_owned();
        match self.event(|e| matches!(e, RunEvent::NodeSettled { node_id, .. } if *node_id == node)).await {
            RunEvent::NodeSettled { state, .. } => state,
            _ => unreachable!(),
        }
    }

    fn state(&self, run_id: &str, node: &str) -> NodeState {
        let snapshot = self.scheduler.snapshot(&self.caller, Some(run_id)).expect("snapshot");
        snapshot.nodes.iter().find(|n| n.id == node).expect("node").state
    }

    async fn tool(&self, params: Value) -> tool::ToolOutcome {
        tool::run(&self.scheduler, &self.caller, &params, None).await
    }
}

fn node(id: &str, deps: &[&str]) -> WorkflowNode {
    WorkflowNode {
        id: id.to_owned(),
        prompt: format!("do {id}"),
        depends_on: deps.iter().map(|d| (*d).to_owned()).collect(),
        category: Some("quick".into()),
        ..WorkflowNode::default()
    }
}

fn def(key: &str, nodes: Vec<WorkflowNode>) -> WorkflowDefinition {
    WorkflowDefinition { key: key.into(), name: format!("flow {key}"), nodes }
}

fn collector() -> (maho_task::Notifier, mpsc::UnboundedReceiver<(String, Value)>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let notifier: maho_task::Notifier = Arc::new(move |kind: &str, text: &str, details: Value| {
        assert_eq!(kind, tool::UPDATE_MESSAGE_TYPE);
        let _ = tx.send((text.to_owned(), details));
    });
    (notifier, rx)
}

#[tokio::test]
async fn acceptance_01_chain_runs_in_order() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("chain", vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])])).expect("start");
    let run = out.run.run_id.clone();
    let a = h.next_start().await;
    assert_eq!(a.node, "a");
    h.no_pending_start();
    assert_eq!(h.state(&run, "b"), NodeState::Blocked);
    h.exec.complete(&a.task);
    let b = h.next_start().await;
    assert_eq!(b.node, "b");
    h.no_pending_start();
    h.exec.complete(&b.task);
    let c = h.next_start().await;
    assert_eq!(c.node, "c");
    h.exec.complete(&c.task);
    assert_eq!(h.settled(&run).await, RunStatus::Completed);
    let snapshot = h.scheduler.snapshot(&h.caller, Some(run.as_str())).expect("snapshot");
    assert_eq!(snapshot.graph.waves.len(), 3);
    assert_eq!(h.scheduler.store().read_result(&run, "c").as_deref(), Some(format!("result of {}", c.task).as_str()));
}

#[tokio::test]
async fn acceptance_02_fan_out_fan_in() {
    let mut h = harness();
    let out = h
        .scheduler
        .start(&h.caller, def("fan", vec![node("a", &[]), node("b1", &["a"]), node("b2", &["a"]), node("b3", &["a"]), node("d", &["b1", "b2", "b3"])]))
        .expect("start");
    let run = out.run.run_id.clone();
    let a = h.next_start().await;
    h.exec.complete(&a.task);
    let mut b = Vec::new();
    for _ in 0..3 {
        b.push(h.next_start().await);
    }
    let mut names: Vec<&str> = b.iter().map(|s| s.node.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["b1", "b2", "b3"]);
    // All three are running at once before any of them completes.
    for id in ["b1", "b2", "b3"] {
        assert_eq!(h.state(&run, id), NodeState::Running);
    }
    h.exec.complete(&b[0].task);
    h.node_settled(&b[0].node).await;
    h.exec.complete(&b[1].task);
    h.node_settled(&b[1].node).await;
    h.no_pending_start();
    assert_ne!(h.state(&run, "d"), NodeState::Running);
    h.exec.complete(&b[2].task);
    let d = h.next_start().await;
    assert_eq!(d.node, "d");
    h.exec.complete(&d.task);
    assert_eq!(h.settled(&run).await, RunStatus::Completed);
}

#[tokio::test]
async fn acceptance_03_two_problems_reported_and_no_run_created() {
    let mut h = harness();
    let outcome = h
        .tool(json!({"action": "start", "definition": {"key": "bad", "name": "bad", "nodes": [
            {"id": "a", "prompt": "x", "category": "quick", "dependsOn": ["a"]},
            {"id": "b", "prompt": "y", "category": "quick", "dependsOn": ["ghost"]}
        ]}}))
        .await;
    assert_eq!(outcome.code(), Some("invalid_definition"));
    let codes: Vec<&str> = outcome.details["errors"].as_array().expect("errors").iter().filter_map(|e| e["code"].as_str()).collect();
    assert_eq!(codes, ["self_dependency", "unknown_dependency"]);
    assert!(h.scheduler.store().list().is_empty());
    h.no_pending_start();
    // Every rule maps to its code at the tool level too (unit coverage in definition.rs).
    let empty = h.tool(json!({"action": "start", "definition": {"key": "e", "name": "e", "nodes": []}})).await;
    assert_eq!(empty.details["errors"][0]["code"], "empty_graph");
}

#[tokio::test]
async fn acceptance_04_cycle_reports_closed_path() {
    let h = harness();
    let outcome = h
        .tool(json!({"action": "start", "definition": {"key": "cyc", "name": "c", "nodes": [
            {"id": "x", "prompt": "p", "category": "q", "dependsOn": ["y"]},
            {"id": "y", "prompt": "p", "category": "q", "dependsOn": ["x"]}
        ]}}))
        .await;
    assert_eq!(outcome.code(), Some("invalid_definition"));
    let error = &outcome.details["errors"][0];
    assert_eq!(error["code"], "cycle");
    let path: Vec<&str> = error["path"].as_array().expect("path").iter().filter_map(Value::as_str).collect();
    assert_eq!(path.len(), 3);
    assert_eq!(path.first(), path.last());
}

#[tokio::test]
async fn acceptance_05_target_rules() {
    let h = harness();
    for node in [
        json!({"id": "a", "prompt": "p", "category": "q", "subagent_type": "task"}),
        json!({"id": "a", "prompt": "p", "category": "q", "model": "p/m"}),
    ] {
        let outcome = h.tool(json!({"action": "start", "definition": {"key": "t", "name": "t", "nodes": [node]}})).await;
        assert_eq!(outcome.code(), Some("invalid_definition"));
        assert_eq!(outcome.details["errors"][0]["code"], maho_workflow::definition::INVALID_TARGET);
        assert_eq!(outcome.details["node_ids"], json!(["a"]));
    }
}

#[tokio::test]
async fn acceptance_06_key_reuse_and_conflict() {
    let mut h = harness();
    let first = h.scheduler.start(&h.caller, def("k", vec![node("a", &[])])).expect("start");
    h.next_start().await;
    let again = h.tool(json!({"action": "start", "definition": def("k", vec![node("a", &[])])})).await;
    assert!(!again.is_error, "{}", again.text);
    assert_eq!(again.details["kind"], "started");
    assert_eq!(again.details["reused"], true);
    assert_eq!(again.details["run_id"], json!(first.run.run_id));
    h.no_pending_start();
    let mut changed = node("a", &[]);
    changed.prompt = "something else".into();
    let conflict = h.tool(json!({"action": "start", "definition": def("k", vec![changed])})).await;
    assert_eq!(conflict.code(), Some("definition_conflict"));
}

#[tokio::test]
async fn acceptance_07_run_limit_and_pruned_key_is_free() {
    let mut h = harness_with(DagSettings { max_runs_per_session: 1, ..DagSettings::default() });
    let first = h.scheduler.start(&h.caller, def("one", vec![node("a", &[])])).expect("start");
    let a = h.next_start().await;
    let error = h.scheduler.start(&h.caller, def("two", vec![node("a", &[])])).expect_err("over the limit");
    assert_eq!(error.code, "run_limit_exceeded");
    h.exec.complete(&a.task);
    h.settled(&first.run.run_id).await;
    // Age the record past retention and sweep it.
    let store = h.scheduler.store().clone();
    let mut record = store.load(&first.run.run_id).expect("record");
    record.finished_at = Some(1);
    store.save(&record).expect("save");
    let (removed, failures) = h.scheduler.sweep();
    assert_eq!(removed, std::slice::from_ref(&first.run.run_id));
    assert!(failures.is_empty());
    let mut changed = node("a", &[]);
    changed.prompt = "new".into();
    let second = h.scheduler.start(&h.caller, def("one", vec![changed])).expect("key is free again");
    assert!(!second.reused);
    assert_ne!(second.run.run_id, first.run.run_id);
}

/// Shared by 8 and 9: `a -> b` plus `c`; `a` fails, `c` completes.
async fn failed_run(h: &mut Harness) -> String {
    let out = h.scheduler.start(&h.caller, def("fail", vec![node("a", &[]), node("b", &["a"]), node("c", &[])])).expect("start");
    let run = out.run.run_id.clone();
    let first = h.next_start().await;
    let second = h.next_start().await;
    let (a, c) = if first.node == "a" { (first, second) } else { (second, first) };
    assert_eq!(c.node, "c");
    h.exec.finish(&a.task, ChildOutcome::Failed { message: "boom".into(), infrastructure: false });
    h.node_settled("a").await;
    h.exec.complete(&c.task);
    assert_eq!(h.settled(&run).await, RunStatus::Failed);
    run
}

#[tokio::test]
async fn acceptance_08_failure_skips_dependents_only() {
    let mut h = harness();
    let run = failed_run(&mut h).await;
    let snapshot = h.scheduler.snapshot(&h.caller, Some(run.as_str())).expect("snapshot");
    assert_eq!(h.state(&run, "c"), NodeState::Completed);
    assert_eq!(h.state(&run, "b"), NodeState::Skipped);
    assert_eq!(h.state(&run, "a"), NodeState::Failed);
    let error = snapshot.error.expect("run error");
    assert_eq!(error.code, "task_error");
    assert_eq!(error.message, "boom");
}

#[tokio::test]
async fn acceptance_09_retry_reruns_failed_and_skipped() {
    let mut h = harness();
    let run = failed_run(&mut h).await;
    let retried = h.tool(json!({"action": "retry", "run_id": run})).await;
    assert_eq!(retried.details["kind"], "retried", "{}", retried.text);
    assert_eq!(retried.details["node_ids"], json!(["a", "b"]));
    let a = h.next_start().await;
    assert_eq!((a.node.as_str(), a.attempt), ("a", 2));
    let snapshot = h.scheduler.snapshot(&h.caller, Some(run.as_str())).expect("snapshot");
    let c = snapshot.nodes.iter().find(|n| n.id == "c").expect("c");
    assert_eq!((c.state, c.attempt), (NodeState::Completed, 1));
    let busy = h.tool(json!({"action": "retry", "run_id": run})).await;
    assert_eq!(busy.code(), Some("run_still_active"));
    h.exec.complete(&a.task);
    let b = h.next_start().await;
    assert_eq!((b.node.as_str(), b.attempt), ("b", 2));
    h.exec.complete(&b.task);
    assert_eq!(h.settled(&run).await, RunStatus::Completed);
}

#[tokio::test]
async fn acceptance_10_retry_prompt_needs_one_node() {
    let mut h = harness();
    let run = failed_run(&mut h).await;
    let outcome = h.tool(json!({"action": "retry", "run_id": run, "node_ids": ["a", "b"], "prompt": "again"})).await;
    assert_eq!(outcome.code(), Some("invalid_arguments"));
    let single = h.tool(json!({"action": "retry", "run_id": run, "node_id": "a", "prompt": "try harder"})).await;
    assert!(!single.is_error, "{}", single.text);
    assert_eq!(h.next_start().await.prompt, "try harder");
}

#[tokio::test(start_paused = true)]
async fn acceptance_11_infrastructure_retries_with_backoff() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("infra", vec![node("a", &[])])).expect("start");
    let run = out.run.run_id.clone();
    let mut task = h.next_start().await.task;
    let mut delays = Vec::new();
    for retry in 1..=2u32 {
        h.exec.finish(&task, ChildOutcome::Failed { message: "host unavailable".into(), infrastructure: true });
        let event = h.event(|e| matches!(e, RunEvent::NodeRetryScheduled { .. })).await;
        let RunEvent::NodeRetryScheduled { retry: n, delay_ms, .. } = event else { unreachable!() };
        assert_eq!(n, retry);
        delays.push(delay_ms);
        let before = tokio::time::Instant::now();
        let restarted = h.next_start().await;
        assert!(before.elapsed() >= Duration::from_millis(delay_ms));
        assert_eq!((restarted.node.as_str(), restarted.attempt), ("a", 1));
        task = restarted.task;
    }
    assert_eq!(delays, [1000, 2000]);
    h.exec.finish(&task, ChildOutcome::Failed { message: "host unavailable".into(), infrastructure: true });
    assert_eq!(h.settled(&run).await, RunStatus::Failed);
    h.no_pending_start();
    let record = h.scheduler.store().load(&run).expect("record");
    assert_eq!(record.nodes["a"].infra_retries, 2);

    let other = h.scheduler.start(&h.caller, def("plain", vec![node("x", &[])])).expect("start");
    let x = h.next_start().await;
    h.exec.finish(&x.task, ChildOutcome::Failed { message: "bad code".into(), infrastructure: false });
    assert_eq!(h.settled(&other.run.run_id).await, RunStatus::Failed);
    h.no_pending_start();
}

#[tokio::test]
async fn acceptance_12_amend() {
    let mut h = harness();
    let original = vec![node("a", &[]), node("b", &["a"]), node("c", &[])];
    let out = h.scheduler.start(&h.caller, def("am", original.clone())).expect("start");
    let run = out.run.run_id.clone();
    for _ in 0..2 {
        let s = h.next_start().await;
        h.exec.complete(&s.task);
    }
    let b = h.next_start().await;
    h.exec.complete(&b.task);
    h.settled(&run).await;

    let mut amended = original.clone();
    amended[0].prompt = "do a differently".into();
    let outcome = h.tool(json!({"action": "amend", "run_id": run, "definition": def("am", amended.clone())})).await;
    assert_eq!(outcome.details["kind"], "amended", "{}", outcome.text);
    assert_eq!(outcome.details["node_ids"], json!(["a", "b"]));
    let a = h.next_start().await;
    assert_eq!((a.node.as_str(), a.prompt.as_str()), ("a", "do a differently"));
    let snapshot = h.scheduler.snapshot(&h.caller, Some(run.as_str())).expect("snapshot");
    let c = snapshot.nodes.iter().find(|n| n.id == "c").expect("c");
    assert_eq!((c.state, c.attempt), (NodeState::Completed, 1));
    assert_eq!(snapshot.amendments, 1);

    let mut again = amended.clone();
    again[0].prompt = "while running".into();
    let running = h.tool(json!({"action": "amend", "run_id": run, "definition": def("am", again)})).await;
    assert_eq!(running.code(), Some("amend_running_node"));
    let other_key = h.tool(json!({"action": "amend", "run_id": run, "definition": def("other", amended)})).await;
    assert_eq!(other_key.code(), Some("invalid_amendment"));
    h.exec.complete(&a.task);
    let b = h.next_start().await;
    assert_eq!(b.node, "b");
    h.exec.complete(&b.task);
    assert_eq!(h.settled(&run).await, RunStatus::Completed);
}

#[tokio::test]
async fn acceptance_13_wait_detached_and_blocking() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("w", vec![node("a", &[]), node("b", &[])])).expect("start");
    let run = out.run.run_id.clone();
    let first = h.next_start().await;
    let second = h.next_start().await;
    let (notifier, mut notices) = collector();
    let waited = tool::run(&h.scheduler, &h.caller, &json!({"action": "wait", "run_id": run}), Some(notifier)).await;
    assert_eq!(waited.details["kind"], "detached", "{}", waited.text);
    assert!(notices.try_recv().is_err());
    h.exec.complete(&first.task);
    h.exec.complete(&second.task);
    let mut kinds = Vec::new();
    for _ in 0..3 {
        let (_, details) = tokio::time::timeout(LIMIT, notices.recv()).await.expect("notice").expect("open");
        kinds.push(details["kind"].as_str().unwrap_or_default().to_owned());
    }
    assert_eq!(kinds, ["node_settled", "node_settled", "run_settled"]);

    let out = h.scheduler.start(&h.caller, def("w2", vec![node("x", &[])])).expect("start");
    let x = h.next_start().await;
    let scheduler = Arc::clone(&h.scheduler);
    let caller = h.caller.clone();
    let run2 = out.run.run_id.clone();
    let blocking = tokio::spawn(async move {
        tool::run(&scheduler, &caller, &json!({"action": "wait", "run_id": run2, "detach": false}), None).await
    });
    h.exec.complete(&x.task);
    let result = tokio::time::timeout(LIMIT, blocking).await.expect("returns after settle").expect("join");
    assert_eq!(result.details["kind"], "waited");
    assert_eq!(result.details["status"], "completed");
    assert_eq!(result.details["results"][0]["output"], json!(format!("result of {}", x.task)));
}

#[tokio::test]
async fn acceptance_14_send() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("s", vec![node("a", &[]), node("b", &["a"])])).expect("start");
    let run = out.run.run_id.clone();
    let a = h.next_start().await;
    let sent = h.tool(json!({"action": "send", "run_id": run, "node_id": "a", "message": "also check docs"})).await;
    assert_eq!(sent.details["kind"], "sent", "{}", sent.text);
    assert_eq!(sent.details["delivery"], "steered");
    assert_eq!(lock(&h.exec.sent).as_slice(), [(a.task.clone(), "also check docs".to_owned())]);
    let none = h.tool(json!({"action": "send", "run_id": run, "node_id": "b", "message": "hi"})).await;
    assert_eq!(none.code(), Some("node_has_no_task"));
}

#[tokio::test]
async fn acceptance_15_cancel_stops_children() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("c", vec![node("a", &[]), node("b", &[]), node("d", &["a", "b"])])).expect("start");
    let run = out.run.run_id.clone();
    let mut tasks = vec![h.next_start().await.task, h.next_start().await.task];
    let outcome = h.tool(json!({"action": "cancel", "run_id": run, "reason": "not needed"})).await;
    assert_eq!(outcome.details["kind"], "cancelled");
    assert_eq!(outcome.details["status"], "cancelled");
    let mut cancelled = lock(&h.exec.cancelled).clone();
    cancelled.sort();
    tasks.sort();
    assert_eq!(cancelled, tasks);
    for id in ["a", "b", "d"] {
        assert_eq!(h.state(&run, id), NodeState::Cancelled);
    }
    h.no_pending_start();
}

#[tokio::test]
async fn acceptance_16_ownership_and_unknown_run() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("o", vec![node("a", &[])])).expect("start");
    h.next_start().await;
    let stranger = Caller::new("someone-else");
    let foreign = tool::run(&h.scheduler, &stranger, &json!({"action": "snapshot", "run_id": out.run.run_id}), None).await;
    assert_eq!(foreign.code(), Some("run_not_owned"));
    assert_eq!(h.tool(json!({"action": "snapshot", "run_id": "wf_nope"})).await.code(), Some("run_not_found"));
    assert_eq!(h.tool(json!({"action": "cancel"})).await.code(), Some("run_not_found"));
}

#[tokio::test]
async fn acceptance_17_skills_prepended_once() {
    let mut h = harness();
    let skill_dir = h.dir.path().join("skills").join("style");
    std::fs::create_dir_all(&skill_dir).expect("mkdir");
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: style\n---\nUse short names.").expect("write");
    let mut a = node("a", &[]);
    a.load_skills = vec!["style".into(), "ghost".into()];
    let out = h.scheduler.start(&h.caller, def("sk", vec![a])).expect("start");
    let started = h.next_start().await;
    assert!(started.prompt.contains("Use short names."), "{}", started.prompt);
    assert!(started.prompt.ends_with("do a"));
    assert_eq!(out.diagnostics.len(), 1);
    assert!(out.diagnostics[0].contains("ghost"));
    std::fs::write(skill_dir.join("SKILL.md"), "---\nname: style\n---\nChanged.").expect("rewrite");
    let record = h.scheduler.store().load(&out.run.run_id).expect("record");
    assert!(record.nodes["a"].prompt.contains("Use short names."));
    assert!(!record.nodes["a"].prompt.contains("Changed."));
}

#[tokio::test]
async fn acceptance_18_recovery_reattaches_live_children() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("rec", vec![node("a", &[]), node("b", &["a"]), node("gone", &[])])).expect("start");
    let run = out.run.run_id.clone();
    let first = h.next_start().await;
    let second = h.next_start().await;
    let (a, gone) = if first.node == "a" { (first, second) } else { (second, first) };
    // The owning process "dies": it stops driving, its record stays on disk.
    h.scheduler.shutdown();
    lock(&h.exec.children).remove(&gone.task);
    let successor = scheduler_at(h.dir.path(), &h.exec, DagSettings::default());
    let mut events = successor.subscribe();
    assert_eq!(successor.recover(&h.caller.session_id), std::slice::from_ref(&run));
    h.exec.complete(&a.task);
    let b = h.next_start().await;
    assert_eq!(b.node, "b");
    h.exec.complete(&b.task);
    let status = tokio::time::timeout(LIMIT, async {
        loop {
            if let Ok(RunEvent::RunSettled { status, .. }) = events.recv().await {
                return status;
            }
        }
    })
    .await
    .expect("settles");
    assert_eq!(status, RunStatus::Failed);
    let snapshot = successor.snapshot(&h.caller, Some(run.as_str())).expect("snapshot");
    let states: HashMap<&str, (NodeState, Option<String>)> = snapshot
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), (n.state, n.error.as_ref().map(|e| e.code.clone()))))
        .collect();
    assert_eq!(states["a"].0, NodeState::Completed);
    assert_eq!(states["b"].0, NodeState::Completed);
    assert_eq!(states["gone"], (NodeState::Failed, Some("resume_task_missing".into())));
}

#[tokio::test]
async fn acceptance_19_retention_spares_running_runs() {
    let mut h = harness();
    let done = h.scheduler.start(&h.caller, def("old", vec![node("a", &[])])).expect("start");
    let a = h.next_start().await;
    h.exec.complete(&a.task);
    h.settled(&done.run.run_id).await;
    let running = h.scheduler.start(&h.caller, def("live", vec![node("x", &[])])).expect("start");
    h.next_start().await;
    let store = h.scheduler.store().clone();
    for id in [&done.run.run_id, &running.run.run_id] {
        let mut record = store.load(id).expect("record");
        record.created_at = 1;
        record.updated_at = 1;
        record.finished_at = record.finished_at.map(|_| 1);
        store.save(&record).expect("save");
    }
    let (removed, _) = h.scheduler.sweep();
    assert_eq!(removed, std::slice::from_ref(&done.run.run_id));
    assert!(store.load(&done.run.run_id).is_none());
    assert!(!store.journal_path(&done.run.run_id).exists());
    assert!(!store.results_dir(&done.run.run_id).exists());
    assert!(store.lookup_key(&h.caller.session_id, "old").is_none());
    assert!(store.load(&running.run.run_id).is_some());
}

#[tokio::test]
async fn acceptance_20_dag_command() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("d", vec![node("a", &[]), node("b", &["a"]), node("c", &["a"])])).expect("start");
    let a = h.next_start().await;
    h.exec.complete(&a.task);
    h.next_start().await;
    h.next_start().await;
    let list = dag::render(&h.scheduler, &h.caller, "");
    assert!(!list.warning);
    assert!(list.text.contains(&out.run.run_id), "{}", list.text);
    assert!(list.text.contains("1/3 done, 2 running"), "{}", list.text);
    let detail = dag::render(&h.scheduler, &h.caller, &out.run.run_id);
    assert!(!detail.warning);
    for needle in ["Wave 1:", "Wave 2:", "Critical path (*): a -> b", "Bottlenecks: a (2)", "category:quick", "after a"] {
        assert!(detail.text.contains(needle), "missing {needle:?} in\n{}", detail.text);
    }
    let missing = dag::render(&h.scheduler, &h.caller, "nope");
    assert!(missing.warning);
    let empty = dag::render(&h.scheduler, &Caller::new("fresh"), "");
    assert!(empty.text.contains("No workflow runs"));
}

#[tokio::test]
async fn acceptance_21_user_paused_run() {
    let mut h = harness();
    let out = h.scheduler.start(&h.caller, def("p", vec![node("a", &[]), node("b", &["a"]), node("c", &[])])).expect("start");
    let run = out.run.run_id.clone();
    h.next_start().await;
    h.next_start().await;
    let not_paused = h.tool(json!({"action": "guide", "run_id": run, "text": "x"})).await;
    assert_eq!(not_paused.code(), Some("run_not_user_paused"));
    let paused = h.scheduler.user_pause(&h.caller, Some(run.as_str())).expect("pause");
    assert_eq!(paused.status, RunStatus::Paused);
    assert_eq!(h.tool(json!({"action": "resume", "run_id": run})).await.code(), Some("run_paused_by_user"));
    let added = h.tool(json!({"action": "guide", "run_id": run, "text": "prefer small diffs", "node_id": "a"})).await;
    assert!(!added.is_error, "{}", added.text);
    let id = added.details["guidance_id"].as_str().expect("id").to_owned();
    let edited = h.tool(json!({"action": "guide", "run_id": run, "guidance_id": id, "text": "prefer tiny diffs"})).await;
    assert!(!edited.is_error, "{}", edited.text);
    let extra = h.tool(json!({"action": "guide", "run_id": run, "text": "drop me"})).await;
    let extra_id = extra.details["guidance_id"].as_str().expect("id").to_owned();
    let removed = h.tool(json!({"action": "guide", "run_id": run, "guidance_id": extra_id, "remove": true})).await;
    assert!(!removed.is_error, "{}", removed.text);
    let gone = h.tool(json!({"action": "guide", "run_id": run, "guidance_id": extra_id, "remove": true})).await;
    assert_eq!(gone.code(), Some("guidance_not_found"));
    let snapshot = h.scheduler.snapshot(&h.caller, Some(run.as_str())).expect("snapshot");
    assert_eq!(snapshot.guidance.len(), 1);
    assert_eq!(snapshot.guidance[0].text, "prefer tiny diffs");
    let cancelled = h.tool(json!({"action": "cancel_node", "run_id": run, "node_id": "c"})).await;
    assert!(!cancelled.is_error, "{}", cancelled.text);
    assert_eq!(cancelled.details["status"], "paused");
    assert_eq!(h.state(&run, "c"), NodeState::Cancelled);
    // The user's resume control delivers the guidance with the node's next dispatch.
    h.scheduler.user_resume(&h.caller, Some(run.as_str())).expect("resume");
    let restarted = h.next_start().await;
    assert_eq!(restarted.node, "a");
    assert!(restarted.prompt.contains("prefer tiny diffs"));
    h.no_pending_start();
}
