//! Run operations behind the `workflow` tool (spec §4) and the user-facing pause/resume control:
//! ownership, attach, snapshot, wait, cancel, retry, send, amend, guide, cancel_node, resume.
use crate::definition::{Limits, WorkflowDefinition, fingerprint, node_fingerprint, validate};
use crate::executor::SendOutcome;
use crate::graph::{GraphInfo, dependency_map, downstream};
use crate::scheduler::{LiveRun, Waker, WorkflowError, WorkflowScheduler, wave_order};
use crate::skills::skill_prefix;
use crate::state::{Guidance, NodeError, NodeRecord, NodeState, RunStatus, WorkflowRun, codes};
use crate::tool::Caller;
use crate::{lock, now_ms};
use maho_task::Notifier;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// A running node with no child activity for this long is reported as stalled.
pub const STALL_MS: u64 = 10 * 60 * 1000;

/// One node in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NodeView {
    pub id: String,
    pub label: String,
    pub state: NodeState,
    pub attempt: u32,
    pub wave: usize,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<NodeError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<u64>,
}

/// A run snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Snapshot {
    pub run_id: String,
    pub key: String,
    pub name: String,
    pub status: RunStatus,
    pub user_paused: bool,
    pub counts: BTreeMap<&'static str, usize>,
    pub total: usize,
    pub nodes: Vec<NodeView>,
    pub graph: GraphInfo,
    /// Running nodes with no child activity for [`STALL_MS`].
    pub stalled: Vec<String>,
    pub amendments: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<NodeError>,
    pub guidance: Vec<Guidance>,
    pub diagnostics: Vec<String>,
    pub warnings: Vec<String>,
}

/// One node's result (`wait` with `detach: false`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NodeResult {
    pub id: String,
    pub state: NodeState,
    pub attempt: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<NodeError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_bytes: Option<usize>,
}

/// What a guide call does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuideOp {
    Add { text: String, node_id: Option<String> },
    Edit { guidance_id: String, text: String },
    Remove { guidance_id: String },
}

fn not_found(run_id: &str) -> WorkflowError {
    WorkflowError::new("run_not_found", format!("no workflow run \"{run_id}\""))
}

fn node_missing(id: &str) -> WorkflowError {
    WorkflowError::new("node_not_found", format!("the run has no node \"{id}\"")).with_nodes(vec![id.to_owned()])
}

impl WorkflowScheduler {
    /// The run `run_id` as seen by `caller` (spec "Run ownership").
    ///
    /// # Errors
    /// `run_not_found` (also for a missing id) or `run_not_owned`.
    pub(crate) fn access(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>) -> Result<Arc<LiveRun>, WorkflowError> {
        let run_id = run_id.map(str::trim).filter(|id| !id.is_empty()).ok_or_else(|| not_found(""))?;
        let live = match self.live_run(run_id) {
            Some(live) => live,
            None => {
                let run = self.store.load(run_id).ok_or_else(|| not_found(run_id))?;
                if run.parent_session != caller.session_id {
                    return Err(WorkflowError::new("run_not_owned", format!("run {run_id} belongs to another session")));
                }
                let elsewhere = !run.status.is_terminal()
                    && run.owner_pid.is_some_and(|pid| pid != std::process::id())
                    && run.owner_pid.is_some_and(maho_task::record::pid_alive);
                if !run.status.is_terminal() && !elsewhere {
                    self.adopt(run)
                } else {
                    self.insert_live(run, !elsewhere)
                }
            }
        };
        if lock(&live.run).parent_session != caller.session_id {
            return Err(WorkflowError::new("run_not_owned", format!("run {run_id} belongs to another session")));
        }
        Ok(live)
    }

    /// Snapshot of a run.
    ///
    /// # Errors
    /// See [`Self::access`].
    pub fn snapshot(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>) -> Result<Snapshot, WorkflowError> {
        let live = self.access(caller, run_id)?;
        let run = lock(&live.run).clone();
        Ok(self.snapshot_of(&run))
    }

    pub(crate) fn snapshot_of(&self, run: &WorkflowRun) -> Snapshot {
        let graph = GraphInfo::of(&run.definition);
        let waves: BTreeMap<&str, usize> = graph
            .waves
            .iter()
            .enumerate()
            .flat_map(|(index, ids)| ids.iter().map(move |id| (id.as_str(), index + 1)))
            .collect();
        let now = now_ms();
        let mut stalled = Vec::new();
        let mut nodes = Vec::new();
        for id in wave_order(&run.definition) {
            let (Some(record), Some(spec)) = (run.nodes.get(&id), run.definition.nodes.iter().find(|n| n.id == id)) else {
                continue;
            };
            let activity = record
                .task_id
                .as_deref()
                .and_then(|task| self.executor.last_activity(task))
                .or(record.last_activity_at);
            if record.state == NodeState::Running && activity.is_some_and(|at| now.saturating_sub(at) >= STALL_MS) {
                stalled.push(id.clone());
            }
            nodes.push(NodeView {
                id: id.clone(),
                label: spec.display_label().to_owned(),
                state: record.state,
                attempt: record.attempt,
                wave: waves.get(id.as_str()).copied().unwrap_or(0),
                target: spec.target_label(),
                model: spec.model.clone(),
                depends_on: spec.depends_on.clone(),
                task_id: record.task_id.clone(),
                started_at: record.started_at,
                finished_at: record.finished_at,
                duration_ms: record.duration_ms(now),
                error: record.error.clone(),
                output_preview: record.output_preview.clone(),
                output_bytes: record.output_bytes,
                last_activity_at: activity,
            });
        }
        Snapshot {
            run_id: run.run_id.clone(),
            key: run.key.clone(),
            name: run.name.clone(),
            status: run.status,
            user_paused: run.user_paused,
            counts: run.state_counts(),
            total: run.nodes.len(),
            nodes,
            graph,
            stalled,
            amendments: run.amendments,
            error: run.error.clone(),
            guidance: run.guidance.clone(),
            diagnostics: run.diagnostics.clone(),
            warnings: run.warnings.clone(),
        }
    }

    /// Registers `notifier` as the wake-up target of a live run. Returns the snapshot and
    /// whether the run is still live.
    ///
    /// # Errors
    /// See [`Self::access`].
    pub fn attach(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>, notifier: Option<Notifier>) -> Result<(Snapshot, bool), WorkflowError> {
        let live = self.access(caller, run_id)?;
        let run = lock(&live.run).clone();
        let active = !run.status.is_terminal();
        if active && let Some(notifier) = notifier {
            let mut waker = lock(&live.waker);
            let each_node = waker.as_ref().is_some_and(|w| w.each_node);
            *waker = Some(Waker { notifier, each_node });
        }
        Ok((self.snapshot_of(&run), active))
    }

    /// Detached wait: a notification per settled node and one for the run. Returns `false` when
    /// the run is already terminal (nothing will be delivered).
    ///
    /// # Errors
    /// See [`Self::access`].
    pub fn wait_detached(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>, notifier: Notifier) -> Result<bool, WorkflowError> {
        let live = self.access(caller, run_id)?;
        if lock(&live.run).status.is_terminal() {
            return Ok(false);
        }
        *lock(&live.waker) = Some(Waker { notifier, each_node: true });
        Ok(true)
    }

    /// Blocks until the run settles; returns its per-node results.
    ///
    /// # Errors
    /// See [`Self::access`].
    pub async fn wait_settled(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>) -> Result<(WorkflowRun, Vec<NodeResult>), WorkflowError> {
        let live = self.access(caller, run_id)?;
        let mut receiver = live.settled.subscribe();
        let _ = receiver.wait_for(|settled| *settled).await;
        let run = lock(&live.run).clone();
        let results = self.results_of(&run);
        Ok((run, results))
    }

    pub(crate) fn results_of(&self, run: &WorkflowRun) -> Vec<NodeResult> {
        wave_order(&run.definition)
            .into_iter()
            .filter_map(|id| run.nodes.get(&id))
            .map(|node| NodeResult {
                id: node.id.clone(),
                state: node.state,
                attempt: node.attempt,
                error: node.error.clone(),
                output: (node.state == NodeState::Completed)
                    .then(|| self.store.read_result(&run.run_id, &node.id))
                    .flatten(),
                output_bytes: node.output_bytes,
            })
            .collect()
    }

    /// Cancels every running child; the run becomes `cancelled`.
    ///
    /// # Errors
    /// See [`Self::access`].
    pub fn cancel(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>, reason: Option<&str>) -> Result<WorkflowRun, WorkflowError> {
        let live = self.access(caller, run_id)?;
        let mut run = lock(&live.run);
        if run.status.is_terminal() {
            return Ok(run.clone());
        }
        let message = reason.map_or_else(|| "the run was cancelled".to_owned(), |r| format!("the run was cancelled: {r}"));
        let now = now_ms();
        for node in run.nodes.values_mut() {
            if node.state.is_terminal() {
                continue;
            }
            if let Some(task) = node.task_id.as_deref().filter(|_| node.state.is_active()) {
                self.executor.cancel(task);
            }
            node.state = NodeState::Cancelled;
            node.error = Some(NodeError::new(codes::TASK_CANCELLED, message.clone()));
            node.finished_at = Some(now);
        }
        run.status = RunStatus::Cancelled;
        run.user_paused = false;
        run.finished_at = Some(now);
        let run_id = run.run_id.clone();
        self.journal(&run_id, "run_cancelled", None, json!({"reason": reason}));
        self.emit(crate::scheduler::RunEvent::RunSettled { run_id: run_id.clone(), status: RunStatus::Cancelled });
        live.settled.send_replace(true);
        let waker = lock(&live.waker).take();
        self.persist(&mut run);
        let snapshot = run.clone();
        drop(run);
        if let Some(waker) = waker {
            (waker.notifier)(
                crate::scheduler::NOTICE_TYPE,
                &format!("Workflow {} ({run_id}) cancelled.", snapshot.name),
                json!({"run_id": run_id, "status": "cancelled", "kind": "run_settled"}),
            );
        }
        Ok(snapshot)
    }

    fn reopen(self: &Arc<Self>, live: &Arc<LiveRun>, run: &mut WorkflowRun) {
        run.status = RunStatus::Running;
        run.finished_at = None;
        run.error = None;
        live.settled.send_replace(false);
        live.driven.store(true, Ordering::SeqCst);
    }

    /// Retries nodes of a settled run in place.
    ///
    /// # Errors
    /// `invalid_arguments`, `run_still_active`, `node_not_found`, `node_not_retryable`.
    pub fn retry(
        self: &Arc<Self>,
        caller: &Caller,
        run_id: Option<&str>,
        node_ids: Option<Vec<String>>,
        prompt: Option<&str>,
    ) -> Result<(WorkflowRun, Vec<String>), WorkflowError> {
        if prompt.is_some() && node_ids.as_ref().is_none_or(|ids| ids.len() != 1) {
            return Err(WorkflowError::new("invalid_arguments", "`prompt` is allowed only when exactly one node is selected"));
        }
        let live = self.access(caller, run_id)?;
        let selected: Vec<String>;
        {
            let mut run = lock(&live.run);
            if !run.status.is_terminal() {
                return Err(WorkflowError::new("run_still_active", format!("run {} is {}; retry needs a settled run", run.run_id, run.status.as_str())));
            }
            let retryable = |state: NodeState| matches!(state, NodeState::Failed | NodeState::Cancelled | NodeState::Skipped);
            let mut chosen: BTreeSet<String> = match &node_ids {
                Some(ids) => {
                    for id in ids {
                        let node = run.nodes.get(id).ok_or_else(|| node_missing(id))?;
                        if !retryable(node.state) {
                            return Err(WorkflowError::new("node_not_retryable", format!("node \"{id}\" is {}", node.state.as_str())).with_nodes(vec![id.clone()]));
                        }
                    }
                    ids.iter().cloned().collect()
                }
                None => run.nodes.values().filter(|n| retryable(n.state)).map(|n| n.id.clone()).collect(),
            };
            if chosen.is_empty() {
                return Err(WorkflowError::new("node_not_retryable", "no failed, cancelled or skipped node to retry"));
            }
            // Dependents that were skipped or cancelled because of a selected node run again too.
            let deps = dependency_map(&run.definition);
            for id in downstream(&deps, &chosen) {
                if run.nodes.get(&id).is_some_and(|n| retryable(n.state)) {
                    chosen.insert(id);
                }
            }
            if let Some(prompt) = prompt
                && let Some(id) = node_ids.as_ref().and_then(|ids| ids.first()).cloned()
            {
                if let Some(node) = run.nodes.get_mut(&id) {
                    node.prompt = format!("{}{prompt}", node.skill_prefix);
                }
                if let Some(spec) = run.definition.nodes.iter_mut().find(|n| n.id == id) {
                    prompt.clone_into(&mut spec.prompt);
                }
                run.fingerprint = fingerprint(&run.definition);
            }
            for id in &chosen {
                if let Some(node) = run.nodes.get_mut(id) {
                    node.reset(true);
                }
            }
            let run_id = run.run_id.clone();
            self.journal(&run_id, "retried", None, json!({"node_ids": chosen}));
            self.reopen(&live, &mut run);
            self.persist(&mut run);
            selected = chosen.into_iter().collect();
        }
        self.pump(&live);
        let run = lock(&live.run).clone();
        Ok((run, selected))
    }

    /// Steers a node's child (or starts a follow-up run in its session).
    ///
    /// # Errors
    /// `invalid_arguments`, `node_not_found`, `node_has_no_task`, `node_not_continuable`.
    pub fn send(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>, node_id: Option<&str>, message: Option<&str>) -> Result<SendOutcome, WorkflowError> {
        let live = self.access(caller, run_id)?;
        let node_id = node_id.ok_or_else(|| WorkflowError::new("invalid_arguments", "send needs `node_id`"))?;
        let message = message
            .filter(|m| !m.trim().is_empty())
            .ok_or_else(|| WorkflowError::new("invalid_arguments", "send needs `message`"))?;
        let task = {
            let run = lock(&live.run);
            let node = run.nodes.get(node_id).ok_or_else(|| node_missing(node_id))?;
            node.task_id.clone().ok_or_else(|| {
                WorkflowError::new("node_has_no_task", format!("node \"{node_id}\" has not started a child task")).with_nodes(vec![node_id.to_owned()])
            })?
        };
        match self.executor.send(&task, message) {
            SendOutcome::NotContinuable => Err(WorkflowError::new("node_not_continuable", format!("the child of node \"{node_id}\" cannot take a message")).with_nodes(vec![node_id.to_owned()])),
            outcome => {
                self.journal(&lock(&live.run).run_id, "node_message", Some(node_id), json!({"bytes": message.len()}));
                Ok(outcome)
            }
        }
    }

    /// Amends a run's definition in place (diff by node id).
    ///
    /// # Errors
    /// `invalid_amendment`, `invalid_definition`, `amend_running_node`.
    pub fn amend(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>, definition: WorkflowDefinition) -> Result<(WorkflowRun, Vec<String>), WorkflowError> {
        let live = self.access(caller, run_id)?;
        let rerun: Vec<String>;
        {
            let mut run = lock(&live.run);
            if definition.key != run.key {
                return Err(WorkflowError::new("invalid_amendment", format!("definition.key \"{}\" does not match the run's key \"{}\"", definition.key, run.key)));
            }
            let issues = validate(&definition, Limits { max_nodes: self.settings.max_nodes_per_run, max_prompt_bytes: self.settings.max_prompt_bytes });
            if !issues.is_empty() {
                let mut error = WorkflowError::new("invalid_definition", format!("the amended definition has {} problem(s)", issues.len()));
                error.errors = issues;
                return Err(error);
            }
            let old: BTreeMap<&str, String> = run.definition.nodes.iter().map(|n| (n.id.as_str(), node_fingerprint(n))).collect();
            let changed: BTreeSet<String> = definition
                .nodes
                .iter()
                .filter(|n| old.get(n.id.as_str()) != Some(&node_fingerprint(n)))
                .map(|n| n.id.clone())
                .collect();
            let new_ids: BTreeSet<&str> = definition.nodes.iter().map(|n| n.id.as_str()).collect();
            let removed: Vec<String> = old.keys().filter(|id| !new_ids.contains(*id)).map(|id| (*id).to_owned()).collect();
            let mut affected = changed.clone();
            affected.extend(downstream(&dependency_map(&definition), &changed));
            let touching: Vec<String> = affected
                .iter()
                .chain(removed.iter())
                .filter(|id| run.nodes.get(id.as_str()).is_some_and(|n| n.state.is_active()))
                .cloned()
                .collect();
            if !touching.is_empty() {
                return Err(WorkflowError::new("amend_running_node", format!("the amendment changes node(s) that are running: {}", touching.join(", "))).with_nodes(touching));
            }
            let mut nodes = BTreeMap::new();
            let mut diagnostics = Vec::new();
            for spec in &definition.nodes {
                let previous = run.nodes.remove(&spec.id);
                let record = match previous {
                    Some(record) if !affected.contains(&spec.id) => record,
                    previous => {
                        let (prefix, missing) = skill_prefix(self.skills.as_ref(), &spec.id, &spec.load_skills);
                        diagnostics.extend(missing);
                        let mut record = NodeRecord::new(&spec.id, format!("{prefix}{}", spec.prompt));
                        record.skill_prefix = prefix;
                        if let Some(previous) = previous {
                            record.attempt = previous.attempt + 1;
                        }
                        record
                    }
                };
                nodes.insert(spec.id.clone(), record);
            }
            run.nodes = nodes;
            run.fingerprint = fingerprint(&definition);
            run.name.clone_from(&definition.name);
            run.definition = definition;
            run.amendments += 1;
            run.diagnostics.extend(diagnostics);
            let run_id = run.run_id.clone();
            self.journal(&run_id, "amended", None, json!({"rerun": affected, "removed": removed}));
            if run.status.is_terminal() && !affected.is_empty() {
                self.reopen(&live, &mut run);
            }
            self.persist(&mut run);
            rerun = affected.into_iter().collect();
        }
        self.pump(&live);
        let run = lock(&live.run).clone();
        Ok((run, rerun))
    }

    /// The user-facing stop control: children are cancelled, their nodes become `paused`, and
    /// the run waits for [`Self::user_resume`].
    ///
    /// # Errors
    /// See [`Self::access`]. A terminal run is returned unchanged.
    pub fn user_pause(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>) -> Result<WorkflowRun, WorkflowError> {
        let live = self.access(caller, run_id)?;
        let mut run = lock(&live.run);
        if run.status.is_terminal() {
            return Ok(run.clone());
        }
        for node in run.nodes.values_mut() {
            if node.state.is_active() {
                if let Some(task) = node.task_id.take() {
                    self.executor.cancel(&task);
                }
                node.state = NodeState::Paused;
            }
        }
        run.status = RunStatus::Paused;
        run.user_paused = true;
        let run_id = run.run_id.clone();
        self.journal(&run_id, "paused_by_user", None, Value::Null);
        self.persist(&mut run);
        Ok(run.clone())
    }

    /// The user-facing resume control: guidance is frozen and handed to the nodes' next
    /// dispatch, paused nodes go back to `pending` and scheduling continues.
    ///
    /// # Errors
    /// `run_not_user_paused`.
    pub fn user_resume(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>) -> Result<WorkflowRun, WorkflowError> {
        let live = self.access(caller, run_id)?;
        {
            let mut run = lock(&live.run);
            if !run.user_paused {
                return Err(WorkflowError::new("run_not_user_paused", "the run was not stopped by the user"));
            }
            for note in &mut run.guidance {
                note.frozen = true;
            }
            self.persist(&mut run);
            let guidance = std::mem::take(&mut run.guidance);
            for note in guidance {
                for node in run.nodes.values_mut() {
                    let targeted = note.node_id.as_deref().is_none_or(|id| id == node.id);
                    if targeted && !node.state.is_terminal() {
                        node.pending_guidance.push(note.text.clone());
                    }
                }
            }
            for node in run.nodes.values_mut() {
                if node.state == NodeState::Paused {
                    node.state = NodeState::Pending;
                }
            }
            run.user_paused = false;
            run.status = RunStatus::Running;
            live.driven.store(true, Ordering::SeqCst);
            let run_id = run.run_id.clone();
            self.journal(&run_id, "resumed_by_user", None, Value::Null);
            self.persist(&mut run);
        }
        self.pump(&live);
        Ok(lock(&live.run).clone())
    }

    /// The `resume` tool action: always refused (only the user resumes a user-stopped run).
    ///
    /// # Errors
    /// `run_paused_by_user` or `run_not_user_paused`.
    pub fn resume_action(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>) -> Result<(), WorkflowError> {
        let live = self.access(caller, run_id)?;
        if lock(&live.run).user_paused {
            return Err(WorkflowError::new("run_paused_by_user", "the user stopped this run; only the user's resume control restarts it"));
        }
        Err(WorkflowError::new("run_not_user_paused", "the run was not stopped by the user; there is nothing to resume"))
    }

    fn paused_run<'a>(live: &'a LiveRun) -> Result<std::sync::MutexGuard<'a, WorkflowRun>, WorkflowError> {
        let run = lock(&live.run);
        if run.user_paused {
            Ok(run)
        } else {
            Err(WorkflowError::new("run_not_user_paused", "this action needs a run stopped by the user"))
        }
    }

    /// Manages guidance notes of a user-paused run; returns the note id affected.
    ///
    /// # Errors
    /// `run_not_user_paused`, `invalid_arguments`, `node_not_found`, `guidance_not_found`,
    /// `guidance_frozen`.
    pub fn guide(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>, op: GuideOp) -> Result<String, WorkflowError> {
        let live = self.access(caller, run_id)?;
        let mut run = Self::paused_run(&live)?;
        let id = match op {
            GuideOp::Add { text, node_id } => {
                if text.trim().is_empty() {
                    return Err(WorkflowError::new("invalid_arguments", "guide needs `text`"));
                }
                if let Some(node) = &node_id
                    && !run.nodes.contains_key(node)
                {
                    return Err(node_missing(node));
                }
                let id = format!("g_{}", uuid::Uuid::new_v4().simple());
                run.guidance.push(Guidance { id: id.clone(), text, node_id, frozen: false });
                id
            }
            GuideOp::Edit { guidance_id, text } => {
                if text.trim().is_empty() {
                    return Err(WorkflowError::new("invalid_arguments", "guide needs `text`"));
                }
                let note = run
                    .guidance
                    .iter_mut()
                    .find(|g| g.id == guidance_id)
                    .ok_or_else(|| WorkflowError::new("guidance_not_found", format!("no guidance \"{guidance_id}\"")))?;
                if note.frozen {
                    return Err(WorkflowError::new("guidance_frozen", "this guidance is already being delivered"));
                }
                note.text = text;
                guidance_id
            }
            GuideOp::Remove { guidance_id } => {
                let index = run
                    .guidance
                    .iter()
                    .position(|g| g.id == guidance_id)
                    .ok_or_else(|| WorkflowError::new("guidance_not_found", format!("no guidance \"{guidance_id}\"")))?;
                if run.guidance[index].frozen {
                    return Err(WorkflowError::new("guidance_frozen", "this guidance is already being delivered"));
                }
                run.guidance.remove(index);
                guidance_id
            }
        };
        let run_id = run.run_id.clone();
        self.journal(&run_id, "guidance_changed", None, json!({"guidance_id": id}));
        self.persist(&mut run);
        Ok(id)
    }

    /// Cancels one node of a user-paused run; the run stays `paused`.
    ///
    /// # Errors
    /// `run_not_user_paused`, `invalid_arguments`, `node_not_found`.
    pub fn cancel_node(self: &Arc<Self>, caller: &Caller, run_id: Option<&str>, node_id: Option<&str>) -> Result<WorkflowRun, WorkflowError> {
        let live = self.access(caller, run_id)?;
        let mut run = Self::paused_run(&live)?;
        let node_id = node_id.ok_or_else(|| WorkflowError::new("invalid_arguments", "cancel_node needs `node_id`"))?;
        let node = run.nodes.get_mut(node_id).ok_or_else(|| node_missing(node_id))?;
        if !node.state.is_terminal() {
            if let Some(task) = node.task_id.as_deref() {
                self.executor.cancel(task);
            }
            node.state = NodeState::Cancelled;
            node.error = Some(NodeError::new(codes::TASK_CANCELLED, "cancelled by the user while the run was paused"));
            node.finished_at = Some(now_ms());
        }
        let run_id = run.run_id.clone();
        self.journal(&run_id, "node_cancelled", Some(node_id), Value::Null);
        self.persist(&mut run);
        Ok(run.clone())
    }
}
