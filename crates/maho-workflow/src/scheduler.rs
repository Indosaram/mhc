//! The frontier scheduler (spec §3): start with key reuse, frontier admission, failure policy
//! (independent branches continue, dependents are skipped), settle, automatic infrastructure
//! retries with exponential backoff, durability (journal + atomic record) and recovery of runs
//! whose owning process ended. The remaining run operations live in [`crate::scheduler_ops`].
use crate::definition::{Limits, WorkflowDefinition, fingerprint, validate};
use crate::executor::{ChildOutcome, ChildProbe, NodeExecutor, NodeRequest};
use crate::graph::{dependency_map, downstream, wave_numbers};
use crate::settings::DagSettings;
use crate::skills::{SkillResolver, skill_prefix};
use crate::state::{
    NodeError, NodeRecord, NodeState, RECORD_VERSION, RunStatus, WorkflowRun, codes, preview,
};
use crate::store::{KeyEntry, ResultStats, WorkflowStore, event};
use crate::tool::Caller;
use crate::{lock, now_ms};
use maho_task::Notifier;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Automatic infrastructure retries per attempt.
pub const MAX_INFRA_RETRIES: u32 = 2;
/// `custom_type` of the notification turns `wait` (detached) triggers.
pub const NOTICE_TYPE: &str = "workflow-update";

/// A scheduler event, broadcast to subscribers (buffer `task.dag.subscriber_ring`; a lagging
/// subscriber is told how many events it missed and keeps going).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunEvent {
    NodeStarted { run_id: String, node_id: String, task_id: String, attempt: u32 },
    NodeRetryScheduled { run_id: String, node_id: String, retry: u32, delay_ms: u64 },
    NodeSettled { run_id: String, node_id: String, state: NodeState },
    RunSettled { run_id: String, status: RunStatus },
}

/// A tool-level error (spec §4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct WorkflowError {
    pub code: String,
    pub message: String,
    pub errors: Vec<crate::definition::ValidationIssue>,
    pub diagnostics: Vec<String>,
    pub node_ids: Vec<String>,
}

impl WorkflowError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            ..Self::default()
        }
    }
    pub fn with_nodes(mut self, ids: Vec<String>) -> Self {
        self.node_ids = ids;
        self
    }
}

/// Result of `start`.
#[derive(Clone, Debug)]
pub struct StartOutcome {
    pub run: WorkflowRun,
    pub reused: bool,
    pub warnings: Vec<String>,
    pub diagnostics: Vec<String>,
}

pub struct SchedulerOptions {
    pub store: WorkflowStore,
    pub settings: DagSettings,
    pub executor: Arc<dyn NodeExecutor>,
    pub skills: Arc<dyn SkillResolver>,
    /// First automatic-retry delay (doubles per retry); the spec's value is 1 s.
    pub retry_base: Duration,
}

pub(crate) struct Waker {
    pub notifier: Notifier,
    /// Notify for every settled node (detached `wait`), not only for the run settle.
    pub each_node: bool,
}

/// A run loaded in this process.
pub struct LiveRun {
    pub(crate) run: Mutex<WorkflowRun>,
    pub(crate) settled: tokio::sync::watch::Sender<bool>,
    pub(crate) waker: Mutex<Option<Waker>>,
    /// Driven (scheduled) by this process.
    pub(crate) driven: AtomicBool,
}

type Notice = (Notifier, String, Value);

pub struct WorkflowScheduler {
    pub(crate) store: WorkflowStore,
    pub(crate) settings: DagSettings,
    pub(crate) executor: Arc<dyn NodeExecutor>,
    pub(crate) skills: Arc<dyn SkillResolver>,
    pub(crate) retry_base: Duration,
    pub(crate) live: Mutex<HashMap<String, Arc<LiveRun>>>,
    events: tokio::sync::broadcast::Sender<RunEvent>,
    /// Events held back until the state change they report is on disk.
    pending_events: Mutex<Vec<RunEvent>>,
    tasks: Mutex<Vec<tokio::task::AbortHandle>>,
    stopped: AtomicBool,
}

impl std::fmt::Debug for WorkflowScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowScheduler")
            .field("root", &self.store.root)
            .finish_non_exhaustive()
    }
}

/// Node ids in wave order (wave, then id).
pub(crate) fn wave_order(definition: &WorkflowDefinition) -> Vec<String> {
    let numbers = wave_numbers(&dependency_map(definition));
    let mut ids: Vec<(usize, String)> = numbers.into_iter().map(|(id, wave)| (wave, id)).collect();
    ids.sort();
    ids.into_iter().map(|(_, id)| id).collect()
}

impl WorkflowScheduler {
    pub fn new(options: SchedulerOptions) -> Arc<Self> {
        let (events, _) = tokio::sync::broadcast::channel(options.settings.subscriber_ring.max(1));
        Arc::new(Self {
            store: options.store,
            settings: options.settings,
            executor: options.executor,
            skills: options.skills,
            retry_base: options.retry_base,
            live: Mutex::default(),
            events,
            pending_events: Mutex::default(),
            tasks: Mutex::default(),
            stopped: AtomicBool::new(false),
        })
    }

    pub fn store(&self) -> &WorkflowStore {
        &self.store
    }

    pub fn settings(&self) -> &DagSettings {
        &self.settings
    }

    /// Subscribes to scheduler events; subscribe before triggering the action you await.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<RunEvent> {
        self.events.subscribe()
    }

    /// Stops driving every run and aborts the child watchers (the process "dies" for its runs;
    /// used by shutdown and by recovery tests). Records are left as they are on disk.
    pub fn shutdown(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        for handle in lock(&self.tasks).drain(..) {
            handle.abort();
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Queues `event`; it is broadcast by the next [`Self::persist`], after the record is saved.
    pub(crate) fn emit(&self, event: RunEvent) {
        lock(&self.pending_events).push(event);
    }

    pub(crate) fn journal(&self, run_id: &str, kind: &str, node: Option<&str>, data: Value) {
        if let Err(error) = self.store.append(run_id, &event(kind, node, data)) {
            maho_ext_api::diagnostic(&format!("workflow {run_id}: cannot append to the journal: {error}"));
        }
    }

    pub(crate) fn persist(&self, run: &mut WorkflowRun) {
        let events: Vec<RunEvent> = lock(&self.pending_events).drain(..).collect();
        if self.is_stopped() {
            return;
        }
        let now = now_ms();
        run.updated_at = now;
        run.lease_at = now;
        run.owner_pid = Some(std::process::id());
        if let Err(error) = self.store.save(run) {
            maho_ext_api::diagnostic(&format!("workflow {}: cannot save the run record: {error}", run.run_id));
        }
        for event in events {
            let _ = self.events.send(event);
        }
    }

    fn spawn<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            maho_ext_api::diagnostic("workflow: no async runtime; a child watcher could not start");
            return;
        };
        let handle = runtime.spawn(future);
        let mut tasks = lock(&self.tasks);
        tasks.retain(|task| !task.is_finished());
        tasks.push(handle.abort_handle());
    }

    pub(crate) fn live_run(&self, run_id: &str) -> Option<Arc<LiveRun>> {
        lock(&self.live).get(run_id).cloned()
    }

    pub(crate) fn insert_live(&self, run: WorkflowRun, driven: bool) -> Arc<LiveRun> {
        let terminal = run.status.is_terminal();
        let live = Arc::new(LiveRun {
            settled: tokio::sync::watch::channel(terminal).0,
            run: Mutex::new(run.clone()),
            waker: Mutex::default(),
            driven: AtomicBool::new(driven),
        });
        lock(&self.live).insert(run.run_id, Arc::clone(&live));
        live
    }

    fn runs_of(&self, parent_session: &str) -> Vec<WorkflowRun> {
        let mut runs: HashMap<String, WorkflowRun> = self
            .store
            .list()
            .into_iter()
            .filter(|run| run.parent_session == parent_session)
            .map(|run| (run.run_id.clone(), run))
            .collect();
        for (id, live) in lock(&self.live).iter() {
            let run = lock(&live.run);
            if run.parent_session == parent_session {
                runs.insert(id.clone(), run.clone());
            }
        }
        runs.into_values().collect()
    }

    /// Runs of `parent_session`, newest first.
    pub fn list(&self, parent_session: &str) -> Vec<WorkflowRun> {
        let mut runs = self.runs_of(parent_session);
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.run_id.cmp(&a.run_id)));
        runs
    }

    /// Spec §3.1: validate, reuse or reject by key, then create and start a run.
    ///
    /// # Errors
    /// `invalid_definition`, `invalid_arguments`, `definition_conflict`, `run_limit_exceeded`.
    pub fn start(self: &Arc<Self>, caller: &Caller, definition: WorkflowDefinition) -> Result<StartOutcome, WorkflowError> {
        if definition.key.trim().is_empty() {
            return Err(WorkflowError::new("invalid_arguments", "definition.key is required"));
        }
        let issues = validate(
            &definition,
            Limits {
                max_nodes: self.settings.max_nodes_per_run,
                max_prompt_bytes: self.settings.max_prompt_bytes,
            },
        );
        if !issues.is_empty() {
            let mut error = WorkflowError::new(
                "invalid_definition",
                format!("the workflow definition has {} problem(s); no run was created", issues.len()),
            );
            error.node_ids = issues.iter().filter_map(|i| i.node_id.clone()).collect::<BTreeSet<_>>().into_iter().collect();
            error.errors = issues;
            return Err(error);
        }
        let print = fingerprint(&definition);
        let key_hash = WorkflowStore::key_hash(&caller.session_id, &definition.key);
        let _guard = self
            .store
            .try_lock(&key_hash, Duration::from_secs(60))
            .ok_or_else(|| WorkflowError::new("definition_conflict", "another start with this key is in progress"))?;
        if let Some(entry) = self.store.lookup_key(&caller.session_id, &definition.key) {
            let existing = self
                .live_run(&entry.run_id)
                .map(|live| lock(&live.run).clone())
                .or_else(|| self.store.load(&entry.run_id));
            if let Some(existing) = existing {
                if existing.fingerprint == print {
                    return Ok(StartOutcome {
                        warnings: existing.warnings.clone(),
                        diagnostics: existing.diagnostics.clone(),
                        run: existing,
                        reused: true,
                    });
                }
                return Err(WorkflowError::new(
                    "definition_conflict",
                    format!(
                        "key \"{}\" already belongs to run {} with a different definition; use amend or a new key",
                        definition.key, existing.run_id
                    ),
                ));
            }
        }
        let count = self.runs_of(&caller.session_id).len();
        if count >= self.settings.max_runs_per_session {
            return Err(WorkflowError::new(
                "run_limit_exceeded",
                format!(
                    "this session already has {count} workflow runs (task.dag.max_runs_per_session = {})",
                    self.settings.max_runs_per_session
                ),
            ));
        }
        let mut diagnostics = Vec::new();
        let mut nodes = std::collections::BTreeMap::new();
        for node in &definition.nodes {
            let (prefix, missing) = skill_prefix(self.skills.as_ref(), &node.id, &node.load_skills);
            diagnostics.extend(missing);
            let mut record = NodeRecord::new(&node.id, format!("{prefix}{}", node.prompt));
            record.skill_prefix = prefix;
            nodes.insert(node.id.clone(), record);
        }
        let warnings = warnings(&definition);
        let now = now_ms();
        let run_id = format!("wf_{}", uuid::Uuid::new_v4().simple());
        let mut run = WorkflowRun {
            version: RECORD_VERSION,
            run_id: run_id.clone(),
            key: definition.key.clone(),
            name: definition.name.clone(),
            parent_session: caller.session_id.clone(),
            root_session: caller.root_session.clone(),
            status: RunStatus::Running,
            fingerprint: print,
            definition,
            nodes,
            created_at: now,
            updated_at: now,
            finished_at: None,
            error: None,
            amendments: 0,
            user_paused: false,
            guidance: Vec::new(),
            diagnostics: diagnostics.clone(),
            warnings: warnings.clone(),
            owner_pid: Some(std::process::id()),
            lease_at: now,
        };
        self.persist(&mut run);
        self.store
            .save_key(&KeyEntry {
                key: run.key.clone(),
                parent_session: run.parent_session.clone(),
                run_id: run_id.clone(),
            })
            .map_err(|error| WorkflowError::new("start_failed", format!("cannot write the key index: {error}")))?;
        self.journal(&run_id, "created", None, json!({"key": run.key, "nodes": run.nodes.len()}));
        let live = self.insert_live(run, true);
        self.pump(&live);
        let run = lock(&live.run).clone();
        Ok(StartOutcome {
            run,
            reused: false,
            warnings,
            diagnostics,
        })
    }

    /// Admits the frontier, applies the failure policy and settles when nothing can progress.
    pub(crate) fn pump(self: &Arc<Self>, live: &Arc<LiveRun>) {
        if self.is_stopped() || !live.driven.load(Ordering::SeqCst) {
            return;
        }
        let mut notices: Vec<Notice> = Vec::new();
        {
            let mut run = lock(&live.run);
            if run.status.is_terminal() || run.status == RunStatus::Paused {
                return;
            }
            let deps = dependency_map(&run.definition);
            let order = wave_order(&run.definition);
            loop {
                let mut progressed = false;
                for id in &order {
                    let Some(state) = run.nodes.get(id).map(|n| n.state) else { continue };
                    if !matches!(state, NodeState::Pending | NodeState::Blocked) {
                        continue;
                    }
                    let ready = deps.get(id).into_iter().flatten().all(|dep| {
                        run.nodes.get(dep).is_some_and(|n| n.state == NodeState::Completed)
                    });
                    if ready {
                        self.dispatch(live, &mut run, id, &mut notices);
                        progressed = true;
                    } else if state == NodeState::Pending
                        && let Some(node) = run.nodes.get_mut(id)
                    {
                        node.state = NodeState::Blocked;
                    }
                }
                if !progressed {
                    break;
                }
            }
            if !run.nodes.values().any(|n| n.state.is_active()) {
                let bad: BTreeSet<String> = run
                    .nodes
                    .values()
                    .filter(|n| n.state.is_terminal() && n.state != NodeState::Completed)
                    .map(|n| n.id.clone())
                    .collect();
                let run_id = run.run_id.clone();
                for id in downstream(&deps, &bad) {
                    if let Some(node) = run.nodes.get_mut(&id)
                        && matches!(node.state, NodeState::Pending | NodeState::Blocked)
                    {
                        node.state = NodeState::Skipped;
                        node.finished_at = Some(now_ms());
                        self.journal(&run_id, "node_skipped", Some(id.as_str()), Value::Null);
                        self.emit(RunEvent::NodeSettled { run_id: run_id.clone(), node_id: id.clone(), state: NodeState::Skipped });
                    }
                }
                if run.nodes.values().all(|n| n.state.is_terminal()) {
                    self.settle(live, &mut run, &mut notices);
                }
            }
            self.persist(&mut run);
        }
        deliver(notices);
    }

    pub(crate) fn settle(&self, live: &LiveRun, run: &mut WorkflowRun, notices: &mut Vec<Notice>) {
        let order = wave_order(&run.definition);
        let first_error = order
            .iter()
            .filter_map(|id| run.nodes.get(id))
            .find(|n| n.state == NodeState::Failed)
            .map(|n| n.error.clone());
        run.status = if let Some(error) = first_error {
            run.error = error;
            RunStatus::Failed
        } else {
            run.error = None;
            RunStatus::Completed
        };
        run.finished_at = Some(now_ms());
        self.journal(&run.run_id, "run_settled", None, json!({"status": run.status.as_str()}));
        self.emit(RunEvent::RunSettled { run_id: run.run_id.clone(), status: run.status });
        live.settled.send_replace(true);
        if let Some(waker) = lock(&live.waker).take() {
            let (done, total, _, failed) = run.counts();
            let mut text = format!(
                "Workflow {} ({}) {}: {done}/{total} nodes completed",
                run.name, run.run_id, run.status.as_str()
            );
            if failed > 0 {
                text.push_str(&format!(", {failed} failed"));
            }
            if let Some(error) = &run.error {
                text.push_str(&format!(" (first failure: {}: {})", error.code, error.message));
            }
            text.push_str(". Use workflow action=snapshot for details.");
            notices.push((waker.notifier, text, json!({"run_id": run.run_id, "status": run.status.as_str(), "kind": "run_settled"})));
        }
    }

    fn dispatch(self: &Arc<Self>, live: &Arc<LiveRun>, run: &mut WorkflowRun, id: &str, notices: &mut Vec<Notice>) {
        let Some(spec) = run.definition.nodes.iter().find(|n| n.id == id).cloned() else { return };
        let run_id = run.run_id.clone();
        let Some(node) = run.nodes.get_mut(id) else { return };
        let mut prompt = node.prompt.clone();
        if !node.pending_guidance.is_empty() {
            prompt.push_str("\n\n# Guidance from the user\n\n");
            prompt.push_str(&node.pending_guidance.join("\n\n"));
            node.pending_guidance.clear();
        }
        let request = NodeRequest {
            run_id: run_id.clone(),
            node_id: id.to_owned(),
            attempt: node.attempt,
            prompt,
            category: spec.category.clone(),
            subagent_type: spec.subagent_type.clone(),
            model: spec.model.clone(),
            label: spec.label.clone(),
        };
        let now = now_ms();
        match self.executor.start(&request) {
            Ok(task_id) => {
                node.state = NodeState::Running;
                node.task_id = Some(task_id.clone());
                node.started_at.get_or_insert(now);
                node.last_activity_at = Some(now);
                self.journal(&run_id, "node_started", Some(id), json!({"task_id": task_id, "attempt": node.attempt}));
                self.emit(RunEvent::NodeStarted { run_id, node_id: id.to_owned(), task_id: task_id.clone(), attempt: node.attempt });
                self.watch(live, id, &task_id);
            }
            Err(error) => {
                node.state = NodeState::Failed;
                node.error = Some(NodeError::new(&error.code, error.message));
                node.finished_at = Some(now);
                self.journal(&run_id, "node_failed", Some(id), json!({"code": error.code}));
                self.emit(RunEvent::NodeSettled { run_id: run_id.clone(), node_id: id.to_owned(), state: NodeState::Failed });
                if let Some(notice) = node_notice(live, run_id.as_str(), node) {
                    notices.push(notice);
                }
            }
        }
    }

    pub(crate) fn watch(self: &Arc<Self>, live: &Arc<LiveRun>, node_id: &str, task_id: &str) {
        let this = Arc::clone(self);
        let live = Arc::clone(live);
        let node_id = node_id.to_owned();
        let task_id = task_id.to_owned();
        let wait = self.executor.wait(&task_id);
        self.spawn(async move {
            let outcome = wait.await;
            this.on_child_settled(&live, &node_id, &task_id, outcome);
        });
    }

    /// Applies a child's outcome to its node (ignored when the node moved on meanwhile).
    pub(crate) fn on_child_settled(self: &Arc<Self>, live: &Arc<LiveRun>, node_id: &str, task_id: &str, outcome: ChildOutcome) {
        if self.is_stopped() {
            return;
        }
        let mut notices = Vec::new();
        {
            let mut run = lock(&live.run);
            let run_id = run.run_id.clone();
            let cancelled_run = run.status == RunStatus::Cancelled;
            let Some(node) = run.nodes.get_mut(node_id) else { return };
            if node.task_id.as_deref() != Some(task_id) || !node.state.is_active() {
                return;
            }
            let now = now_ms();
            node.last_activity_at = Some(now);
            let infrastructure = matches!(
                outcome,
                ChildOutcome::Failed { infrastructure: true, .. } | ChildOutcome::Interrupted { .. }
            );
            if infrastructure && !cancelled_run && node.infra_retries < MAX_INFRA_RETRIES {
                node.infra_retries += 1;
                let retry = node.infra_retries;
                let attempt = node.attempt;
                node.state = NodeState::Scheduled;
                node.task_id = None;
                let delay = self.retry_base.saturating_mul(1 << (retry - 1));
                let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
                self.journal(&run_id, "node_retry_scheduled", Some(node_id), json!({"retry": retry, "delay_ms": delay_ms}));
                self.emit(RunEvent::NodeRetryScheduled { run_id, node_id: node_id.to_owned(), retry, delay_ms });
                self.persist(&mut run);
                drop(run);
                let this = Arc::clone(self);
                let live = Arc::clone(live);
                let node_id = node_id.to_owned();
                self.spawn(async move {
                    tokio::time::sleep(delay).await;
                    this.redispatch(&live, &node_id, attempt, retry);
                });
                return;
            }
            match outcome {
                ChildOutcome::Completed { output } => {
                    let stats = ResultStats { bytes: output.len(), attempt: node.attempt, task_id: Some(task_id.to_owned()), finished_at: now };
                    if let Err(error) = self.store.write_result(&run_id, node_id, &output, &stats) {
                        maho_ext_api::diagnostic(&format!("workflow {run_id}: cannot store the result of {node_id}: {error}"));
                    }
                    node.output_preview = Some(preview(&output));
                    node.output_bytes = Some(output.len());
                    node.state = NodeState::Completed;
                }
                ChildOutcome::Failed { message, infrastructure } => {
                    let code = if infrastructure { codes::TASK_LOST } else { codes::TASK_ERROR };
                    node.error = Some(NodeError::new(code, message));
                    node.state = NodeState::Failed;
                }
                ChildOutcome::Interrupted { message } => {
                    node.error = Some(NodeError::new(codes::TASK_INTERRUPTED, message));
                    node.state = NodeState::Failed;
                }
                ChildOutcome::Cancelled => {
                    node.error = Some(NodeError::new(codes::TASK_CANCELLED, "the child task was cancelled"));
                    node.state = NodeState::Cancelled;
                }
            }
            node.finished_at = Some(now);
            let state = node.state;
            self.journal(&run_id, "node_settled", Some(node_id), json!({"state": state.as_str()}));
            self.emit(RunEvent::NodeSettled { run_id: run_id.clone(), node_id: node_id.to_owned(), state });
            if let Some(notice) = run.nodes.get(node_id).and_then(|node| node_notice(live, &run_id, node)) {
                notices.push(notice);
            }
            self.persist(&mut run);
        }
        deliver(notices);
        self.pump(live);
    }

    fn redispatch(self: &Arc<Self>, live: &Arc<LiveRun>, node_id: &str, attempt: u32, retry: u32) {
        if self.is_stopped() {
            return;
        }
        let mut notices = Vec::new();
        {
            let mut run = lock(&live.run);
            let status = run.status;
            let Some(node) = run.nodes.get_mut(node_id) else { return };
            if node.state != NodeState::Scheduled || node.attempt != attempt || node.infra_retries != retry || node.task_id.is_some() {
                return;
            }
            if status != RunStatus::Running {
                node.state = NodeState::Pending;
                self.persist(&mut run);
                return;
            }
            self.dispatch(live, &mut run, node_id, &mut notices);
            self.persist(&mut run);
        }
        deliver(notices);
        self.pump(live);
    }

    fn owner_gone(&self, run: &WorkflowRun) -> bool {
        match run.owner_pid {
            None => true,
            Some(pid) if pid == std::process::id() => self.live_run(&run.run_id).is_none(),
            Some(pid) => owner_dead(pid, run.lease_at, self.settings.heartbeat_ms),
        }
    }

    /// Spec §3.7: takes over the unsettled runs of `parent_session` whose owning process ended;
    /// live children are re-attached, vanished ones mark their node. Returns the recovered ids.
    pub fn recover(self: &Arc<Self>, parent_session: &str) -> Vec<String> {
        let mut recovered = Vec::new();
        for run in self.store.list() {
            if run.parent_session != parent_session || run.status.is_terminal() || !self.owner_gone(&run) {
                continue;
            }
            recovered.push(run.run_id.clone());
            self.adopt(run);
        }
        recovered
    }

    pub(crate) fn adopt(self: &Arc<Self>, run: WorkflowRun) -> Arc<LiveRun> {
        let live = self.insert_live(run, true);
        let mut settled = Vec::new();
        {
            let mut run = lock(&live.run);
            let run_id = run.run_id.clone();
            let paused = run.user_paused;
            self.journal(&run_id, "recovered", None, json!({"pid": std::process::id()}));
            let ids: Vec<String> = run.nodes.keys().cloned().collect();
            for id in ids {
                let Some(node) = run.nodes.get_mut(&id) else { continue };
                if !node.state.is_active() {
                    continue;
                }
                let Some(task_id) = node.task_id.clone() else {
                    // Waiting for an automatic retry when the owner died: dispatch again.
                    node.state = NodeState::Pending;
                    continue;
                };
                let lost = |node: &mut NodeRecord, code: &str, message: &str| {
                    node.state = NodeState::Failed;
                    node.error = Some(NodeError::new(if paused { codes::LOST_WHILE_PAUSED } else { code }, message));
                    node.finished_at = Some(now_ms());
                };
                match self.executor.probe(&task_id) {
                    ChildProbe::Running => self.watch(&live, &id, &task_id),
                    ChildProbe::Settled(outcome) => settled.push((id.clone(), task_id, outcome)),
                    ChildProbe::Lost => lost(node, codes::TASK_LOST, "the child task ended without a result while its owner was gone"),
                    ChildProbe::Missing => lost(node, codes::RESUME_TASK_MISSING, "the child task no longer exists"),
                }
            }
            self.persist(&mut run);
        }
        for (id, task_id, outcome) in settled {
            self.on_child_settled(&live, &id, &task_id, outcome);
        }
        self.pump(&live);
        live
    }

    /// Refreshes the lease of every run driven here (call every `task.dag.heartbeat_ms`).
    pub fn heartbeat(&self) {
        let lives: Vec<Arc<LiveRun>> = lock(&self.live).values().cloned().collect();
        for live in lives {
            if !live.driven.load(Ordering::SeqCst) {
                continue;
            }
            let mut run = lock(&live.run);
            if !run.status.is_terminal() {
                self.persist(&mut run);
            }
        }
    }

    /// Spec §3.8: removes terminal runs older than `task.dag.retention_days`.
    pub fn sweep(&self) -> (Vec<String>, Vec<String>) {
        let (removed, failures) = self.store.sweep(self.settings.retention_days, now_ms());
        let mut live = lock(&self.live);
        for id in &removed {
            live.remove(id);
        }
        (removed, failures)
    }
}

#[cfg(unix)]
fn owner_dead(pid: u32, _lease_at: u64, _heartbeat_ms: u64) -> bool {
    !maho_task::record::pid_alive(pid)
}

/// Windows: no pid check; the owner is gone when its lease is older than three heartbeats.
#[cfg(not(unix))]
fn owner_dead(_pid: u32, lease_at: u64, heartbeat_ms: u64) -> bool {
    now_ms().saturating_sub(lease_at) > heartbeat_ms.saturating_mul(3)
}

fn node_notice(live: &LiveRun, run_id: &str, node: &NodeRecord) -> Option<Notice> {
    let waker = lock(&live.waker);
    let waker = waker.as_ref().filter(|w| w.each_node)?;
    let mut text = format!("Workflow {run_id}: node {} {}", node.id, node.state.as_str());
    if let Some(error) = &node.error {
        text.push_str(&format!(" ({}: {})", error.code, error.message));
    }
    if let Some(preview) = &node.output_preview {
        let short: String = preview.chars().take(300).collect();
        text.push_str(&format!("\n{short}"));
    }
    Some((
        Arc::clone(&waker.notifier),
        text,
        json!({"run_id": run_id, "node_id": node.id, "state": node.state.as_str(), "kind": "node_settled"}),
    ))
}

fn deliver(notices: Vec<Notice>) {
    for (notifier, text, details) in notices {
        notifier(NOTICE_TYPE, &text, details);
    }
}

/// Advisory warnings (never block a start).
fn warnings(definition: &WorkflowDefinition) -> Vec<String> {
    let mut warnings = Vec::new();
    for node in &definition.nodes {
        let unique: BTreeSet<&String> = node.depends_on.iter().collect();
        if unique.len() != node.depends_on.len() {
            warnings.push(format!("node \"{}\" lists a dependency more than once", node.id));
        }
        if node.prompt.trim().is_empty() {
            warnings.push(format!("node \"{}\" has an empty prompt", node.id));
        }
    }
    warnings
}
