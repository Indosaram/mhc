//! Durable session lifecycle controller, shared by session front ends.
use crate::lifecycle_children::LifecycleChildren;
use crate::lifecycle_ledger::{LedgerUpdate, LifecycleLedger};
use crate::lifecycle_stop::{DeadlineStopCapture, RunStopFacts, same_stop, stop_detail};
use crate::lifecycle_wait_items::{WaitSourceItems, project_wait_items};
use crate::session_activity::SessionActivitySnapshot;
use crate::session_lifecycle_types::*;
use serde_json::{Value, json};
use std::sync::Arc;

/// The `session_hold` key a reopened session installs, before session_start producers run, when its last run
/// was interrupted: opening a session is not explicit Resume, so automatic work waits for human input or Resume.
pub use crate::engine_turn_hold::INTERRUPTED_RUN_HOLD_KEY;

/// Children reports a parent's waits name; the rest are counted in `childReportsOmitted`.
const MAX_CHILD_REPORTS: usize = 32;

#[derive(Clone, Debug)]
struct OutstandingTool {
    tool_name: String,
    tool_call_id: String,
    since: String,
}

/// Live run activity (senpi `SessionLifecycleController` `outstandingTools`/`modelSince`): the oldest outstanding
/// top-level tool, else the model stream in progress. Wire-only; durable records never carry it.
#[derive(Clone, Debug, Default)]
pub struct LifecycleActivity {
    /// Insertion-ordered: the first entry is the oldest outstanding top-level tool.
    outstanding_tools: Vec<OutstandingTool>,
    model_since: Option<String>,
}
impl LifecycleActivity {
    pub fn clear(&mut self) {
        self.activity.clear();
    }
    /// Returns whether the event changed the live activity.
    pub fn observe(&mut self, event: &maho_agent::types::AgentEvent) -> bool {
        use maho_agent::types::AgentEvent;
        match event {
            AgentEvent::ToolExecutionStart {
                tool_call_id,
                tool_name,
                parent_tool_call_id: None,
                ..
            } => {
                if let Some(tool) = self
                    .outstanding_tools
                    .iter_mut()
                    .find(|tool| &tool.tool_call_id == tool_call_id)
                {
                    tool.tool_name = tool_name.clone();
                    tool.since = now_iso();
                } else {
                    self.outstanding_tools.push(OutstandingTool {
                        tool_name: tool_name.clone(),
                        tool_call_id: tool_call_id.clone(),
                        since: now_iso(),
                    });
                }
            }
            AgentEvent::ToolExecutionEnd {
                tool_call_id,
                parent_tool_call_id: None,
                ..
            } => self
                .outstanding_tools
                .retain(|tool| &tool.tool_call_id != tool_call_id),
            // Only the provider stream start is live model activity; prepared/synthetic starts are not.
            AgentEvent::MessageStart {
                message,
                streaming: Some(true),
            } if message.role() == "assistant" => {
                self.model_since = Some(now_iso());
            }
            AgentEvent::MessageEnd { message } if message.role() == "assistant" => {
                self.model_since = None;
            }
            AgentEvent::AgentEnd { .. } => self.clear(),
            _ => return false,
        }
        true
    }
    /// The live `activity` a running lifecycle projects; `None` when the state is not running or nothing is live.
    pub fn project(&self, state: &str) -> Option<Value> {
        if state != "running" {
            return None;
        }
        match self.outstanding_tools.first() {
            Some(oldest) => Some(
                json!({"kind":"tool","toolName":oldest.tool_name,"toolCallId":oldest.tool_call_id,"since":oldest.since,"outstanding":self.outstanding_tools.len()}),
            ),
            None => self
                .model_since
                .as_ref()
                .map(|since| json!({"kind":"model","since":since})),
        }
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// `projectWaitItems(record)`: the named items a custody record or source carries, in wire form.
fn project_record_items(record: &Value, wait: &mut Value) {
    let detail: WaitSourceItems = serde_json::from_value(json!({
        "items": record.get("items").cloned().unwrap_or(Value::Null),
        "omittedItems": record.get("omittedItems").cloned().unwrap_or(Value::Null),
    }))
    .unwrap_or_default();
    let projected = project_wait_items(&detail);
    if let Some(items) = projected.items {
        wait["items"] = json!(items);
    }
    if let Some(omitted) = projected.omitted_items {
        wait["omittedItems"] = json!(omitted);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LifecycleRuntimeFacts {
    pub activity: SessionActivitySnapshot,
    pub pending_messages: usize,
    pub holds: Vec<LifecycleHold>,
    pub human: Vec<HumanWait>,
    pub input_admission_pending: bool,
}
pub type LifecycleAppend = Arc<dyn Fn(&Value) -> Result<(), String> + Send + Sync>;
pub struct SessionLifecycleOptions {
    pub session_id: String,
    pub spawned_by: Option<SpawnedBy>,
    pub read_runtime: Arc<dyn Fn() -> LifecycleRuntimeFacts + Send + Sync>,
    pub append_child: Option<Arc<dyn Fn(ChildLifecycleReport) + Send + Sync>>,
    pub entries: Vec<Value>,
    pub append: LifecycleAppend,
    pub custody: Box<dyn crate::work_obligations::CustodyStore>,
}
struct ControllerOptions {
    session_id: String,
    read_runtime: Arc<dyn Fn() -> LifecycleRuntimeFacts + Send + Sync>,
    append_child: Option<Arc<dyn Fn(ChildLifecycleReport) + Send + Sync>>,
    append: LifecycleAppend,
}
pub struct SessionLifecycleController {
    goal_service: Arc<maho_ext_api::goal_lifecycle::GoalLifecycleService>,
    options: ControllerOptions,
    pub ledger: LifecycleLedger,
    pub obligations: crate::work_obligations::SessionWorkObligations,
    pub children: LifecycleChildren,
    idle_boundary_passed: bool,
    pending_human: bool,
    admitting_human: bool,
    run_admitted: bool,
    goal: Option<Value>,
    liveness: Option<Value>,
    stop_facts: Option<RunStopFacts>,
    deadline_stop: Option<DeadlineStopCapture>,
    activity: LifecycleActivity,
    /// A run that ended before this process keeps exactly the stop its record carries; none is derived for it.
    restored_run_id: Option<String>,
}
impl SessionLifecycleController {
    pub fn new(options: SessionLifecycleOptions) -> Self {
        let spawned = options
            .spawned_by
            .as_ref()
            .map(|s| serde_json::to_value(s).expect("spawned by"));
        let ledger = LifecycleLedger::new(&options.session_id, spawned.as_ref(), &options.entries);
        let children = LifecycleChildren::new(&options.entries);
        let liveness = ledger.last_turn_end.clone();
        let restored_run_id = ledger.run_id.clone();
        // Move the custody store out while retaining the remaining immutable options.
        let SessionLifecycleOptions {
            session_id,
            spawned_by: _,
            read_runtime,
            append_child,
            entries: _,
            append,
            custody,
        } = options;
        let obligations = crate::work_obligations::SessionWorkObligations::new(custody);
        Self {
            goal_service: maho_ext_api::goal_lifecycle::goal_lifecycle_service(&session_id),
            options: ControllerOptions {
                session_id,
                read_runtime,
                append_child,
                append,
            },
            ledger,
            obligations,
            children,
            idle_boundary_passed: false,
            pending_human: false,
            admitting_human: false,
            run_admitted: false,
            goal: None,
            liveness,
            stop_facts: None,
            deadline_stop: None,
            activity: LifecycleActivity::default(),
            restored_run_id,
        }
    }
    fn checkpoint(&mut self) {
        if self.obligations.flush().is_err() {
            self.ledger.custody_write_failed();
            return;
        }
        let append = self.options.append.clone();
        self.ledger.checkpoint(|r| append(r));
    }
    pub fn snapshot(&mut self) -> SessionLifecycle {
        self.recompute();
        serde_json::from_value(self.ledger.snapshot().clone())
            .expect("validated lifecycle snapshot")
    }
    pub fn requires_human_continuation(&self) -> bool {
        self.ledger.interrupted && !self.pending_human
    }
    /// The hold key to install when this session is opened, if its run must wait for a person.
    pub fn opening_hold_key(&self) -> Option<&'static str> {
        self.requires_human_continuation()
            .then_some(INTERRUPTED_RUN_HOLD_KEY)
    }
    /// Binding restore: consumed work is discovered before session_start can queue another automatic turn. A
    /// custody record with a ticket that was consumed, or whose delivery the transcript already holds, marks the
    /// run interrupted; the later recovery pass still writes interruption/tool records.
    pub fn mark_interrupted_from_custody(&mut self, delivered: impl Fn(&str) -> bool) {
        let consumed = self.obligations.active().into_iter().any(|record| {
            record.ticket_id.as_deref().is_some_and(|ticket| {
                record.state == crate::work_custody_record::CustodyState::Consumed
                    || delivered(ticket)
            })
        });
        if consumed {
            self.ledger.interrupted = true;
        }
    }
    pub fn next_run_origin(&self) -> EpochOrigin {
        if self.pending_human {
            EpochOrigin::Human
        } else {
            EpochOrigin::Automatic
        }
    }
    pub fn restore_complete(&mut self) {
        self.ledger.release_restore_barrier();
        self.idle_boundary_passed = true;
        self.recompute();
        if self.ledger.current["seq"].as_u64().unwrap_or(0) > 0 {
            self.checkpoint();
        }
    }
    pub fn human_input(&mut self, at: &str) {
        self.admitting_human = true;
        self.pending_human |= !self.ledger.run_active;
        let append = self.options.append.clone();
        self.ledger.human_input(at, |r| append(r));
    }
    /// A turn was admitted and its run is about to start.
    pub fn note_run_admitted(&mut self) {
        self.run_admitted = true;
    }
    pub fn admission_settled(&mut self) {
        self.admitting_human = false;
        if !self.ledger.run_active
            && !self.ledger.interrupted
            && (self.options.read_runtime)().pending_messages == 0
        {
            self.pending_human = false;
        }
        // An admitted run publishes its own opening record when it starts (senpi#3980).
        if self.run_admitted {
            return;
        }
        self.recompute();
    }
    pub fn start_run(&mut self, origin: Option<EpochOrigin>) {
        self.activity.clear();
        self.admitting_human = false;
        self.run_admitted = false;
        self.idle_boundary_passed = false;
        self.liveness = None;
        self.stop_facts = None;
        self.deadline_stop = None;
        let interrupted = self
            .ledger
            .interrupted
            .then(|| self.ledger.run_id.clone())
            .flatten();
        self.ledger
            .start_run(match origin.unwrap_or(self.next_run_origin()) {
                EpochOrigin::Human => "human",
                EpochOrigin::Automatic => "automatic",
            });
        self.pending_human = false;
        if let Some(id) = interrupted {
            self.obligations.settle_run(&id);
        }
        self.recompute();
        self.publish_unwritten();
    }
    fn publish_unwritten(&mut self) {
        if self.obligations.flush().is_err() {
            self.ledger.custody_write_failed();
            return;
        }
        let append = self.options.append.clone();
        self.ledger.publish_unwritten(|r| append(r));
    }
    pub fn idle(&mut self) {
        self.activity.clear();
        self.run_admitted = false;
        self.ledger.run_active = false;
        self.idle_boundary_passed = true;
        if let Some(id) = self.ledger.run_id.clone() {
            self.obligations.settle_run(&id);
        }
        self.recompute();
        self.checkpoint();
    }
    /// Synchronous core boundaries only; token/progress events never cause activity writes.
    pub fn observe_activity(&mut self, event: &maho_agent::types::AgentEvent) {
        if !self.ledger.run_active {
            return;
        }
        if self.activity.observe(event) {
            self.recompute();
        }
    }
    /// The turn deadline fired: captured before its durable write, for the run it stopped only.
    pub fn deadline_stopped(&mut self, kind: LifecycleDeadlineKind, at_ms: i64) {
        if let Some(run_id) = self
            .ledger
            .run_id
            .clone()
            .filter(|_| self.ledger.run_active)
        {
            self.deadline_stop = Some(DeadlineStopCapture {
                run_id,
                kind,
                at: chrono::DateTime::from_timestamp_millis(at_ms)
                    .unwrap_or_default()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                persisted: None,
            });
        }
    }
    /// The deadline-stop entry was refused; the captured stop says so, and the snapshot's `persisted` is untouched.
    pub fn deadline_stop_write_failed(&mut self) {
        if let Some(stop) = self.deadline_stop.as_mut() {
            stop.persisted = Some(false);
        }
    }
    pub fn goal_facts(&mut self, goal: Option<Value>, input: Option<Value>) {
        self.goal_facts_with_stop(goal, input, None);
    }
    pub fn goal_facts_with_stop(
        &mut self,
        goal: Option<Value>,
        input: Option<Value>,
        stop: Option<RunStopFacts>,
    ) {
        self.stop_facts = stop;
        self.liveness = input.clone();
        self.ledger.last_turn_end = input;
        self.goal_changed(goal);
    }
    pub fn goal_changed(&mut self, goal: Option<Value>) {
        self.goal_service.transition(goal.as_ref().map(|g| {
            maho_ext_api::goal_lifecycle::LifecycleGoal {
                id: g["id"].as_str().expect("goal id").to_owned(),
                status: g["status"].as_str().expect("goal status").to_owned(),
                continuation_stopped: g.get("continuationStoppedAt").is_some(),
            }
        }));
        if let Some(g) = goal.as_ref().filter(|g| g["status"] == "complete")
            && self
                .goal
                .as_ref()
                .is_none_or(|old| old["status"] != "complete")
            && let Some(id) = g["id"].as_str()
        {
            let append = self.options.append.clone();
            self.ledger.mark_goal_completed(id, |r| append(r));
        }
        self.goal = goal;
        self.recompute();
    }
    pub fn report_child(&mut self, report: ChildLifecycleReport) {
        let value = serde_json::to_value(&report).expect("child report");
        let append = self.options.append_child.clone();
        let result = self.children.report(&value, |v| {
            if let Some(append) = append {
                append(serde_json::from_value(v.clone()).expect("child report"));
            }
            Ok(())
        });
        if result.is_err() {
            self.ledger.custody_write_failed();
        }
        self.recompute();
    }
    pub fn mark_run_completed(&mut self, id: &str) {
        let append = self.options.append.clone();
        self.ledger.mark_run_completed(id, |r| append(r));
        self.recompute();
    }
    pub fn request_self_settle(&mut self) -> SelfSettleResult {
        if self.admitting_human || (self.options.read_runtime)().input_admission_pending {
            return SelfSettleResult {
                recorded: false,
                reason: Some(SelfSettleReason::NotEligible),
                epoch: self.ledger.current["epoch"].as_u64().unwrap_or(0),
                human_input_revision: self.ledger.current["humanInputRevision"]
                    .as_u64()
                    .unwrap_or(0),
            };
        }
        self.recompute();
        let append = self.options.append.clone();
        serde_json::from_value(self.ledger.request_self_settle(|r| append(r)))
            .expect("settle result")
    }
    pub fn recompute(&mut self) {
        let shared = self.goal_service.snapshot();
        if let Some(id) = shared.completed_goal_id.as_deref() {
            let append = self.options.append.clone();
            self.ledger.mark_goal_completed(id, |r| append(r));
        }
        self.goal = None;
        if let Some(goal) = shared.goal {
            let mut value = json!({"id":goal.id,"status":goal.status});
            if goal.continuation_stopped {
                value["continuationStoppedAt"] = json!(true);
            }
            self.goal = Some(value);
        }
        if shared.input.is_some() {
            self.liveness = shared.input;
        }

        if !self.obligations.persisted() {
            self.ledger.custody_write_failed();
        }
        if self.admitting_human || self.ledger.current["restoring"] == true {
            return;
        }
        let runtime = (self.options.read_runtime)();
        let active = self
            .obligations
            .active()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let reports = self.children.reports();
        let own = reports.iter().find(|r| {
            r["childSessionId"] == self.options.session_id
                && r["taskId"] == self.ledger.current["taskId"]
        });
        let mut child_records = reports
            .iter()
            .filter(|r| {
                r["childSessionId"] != self.options.session_id && r.get("terminal").is_none()
            })
            .collect::<Vec<_>>();
        child_records.sort_by(|a, b| {
            a["taskId"]
                .as_str()
                .unwrap_or_default()
                .cmp(b["taskId"].as_str().unwrap_or_default())
        });
        let child_reports = child_records
            .iter()
            .take(MAX_CHILD_REPORTS)
            .map(|r| {
                let mut report = json!({"taskId":r["taskId"],"childSessionId":r["childSessionId"],"runEpoch":r["runEpoch"],"state":r["state"]});
                for k in ["label", "since"] {
                    if let Some(v) = r.get(k).filter(|v| !v.is_null()) {
                        report[k] = v.clone();
                    }
                }
                report
            })
            .collect::<Vec<_>>();
        let project = |r: &crate::work_custody_record::WorkCustodyRecord| {
            let mut wait = json!({"source":r.source.as_deref().unwrap_or(&r.key),"activeCount":r.active_count.unwrap_or(1.) as u64});
            project_record_items(&serde_json::to_value(r).unwrap_or_default(), &mut wait);
            wait
        };
        let child_sources = active
            .iter()
            .filter(|r| r.kind == crate::work_custody_record::ObligationKind::Children)
            .map(project)
            .collect::<Vec<_>>();
        let children = child_records.len().max(
            active
                .iter()
                .filter(|r| r.kind == crate::work_custody_record::ObligationKind::Children)
                .map(|r| r.active_count.unwrap_or(1.) as usize)
                .sum(),
        );
        let background = active
            .iter()
            .filter(|r| r.kind == crate::work_custody_record::ObligationKind::Background)
            .map(project)
            .collect();
        let facts = evaluate_turn_facts(
            self.liveness.as_ref(),
            self.goal.as_ref(),
            background,
            (runtime.pending_messages, children),
            (self.ledger.run_active, self.ledger.interrupted),
            &self.ledger.current,
            runtime.activity.is_streaming
                || runtime.activity.is_compacting
                || runtime.activity.is_bash_running
                || runtime.activity.has_session_work,
        );
        let mut human = if self.ledger.current["role"] == "root" {
            runtime
                .human
                .iter()
                .map(|h| serde_json::to_value(h).expect("human wait"))
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        human.extend(self.ledger.attention.reconcile(&facts.synthetic));
        let mut waits = json!({"children":children,"background":facts.background,"human":human});
        if !child_sources.is_empty() {
            waits["childSources"] = json!(child_sources);
        }
        if !child_reports.is_empty() {
            waits["childReports"] = json!(child_reports);
        }
        if child_records.len() > MAX_CHILD_REPORTS {
            waits["childReportsOmitted"] = json!(child_records.len() - MAX_CHILD_REPORTS);
        }
        let stalled = facts.synthetic.iter().any(|(_, kind)| kind == "stalled");
        let last_stop = self.last_stop(self.liveness.as_ref(), stalled);
        let holds = runtime
            .holds
            .iter()
            .map(|h| serde_json::to_value(h).expect("hold"))
            .collect::<Vec<_>>();
        let terminal = own.and_then(|r| r["terminal"].as_str());
        let state = crate::lifecycle_runtime_facts::lifecycle_state(
            self.ledger.current["role"].as_str().expect("role"),
            terminal,
            &holds,
            &waits,
            facts.running,
            facts.failure.is_some(),
        );
        if ["done", "failed", "cancelled"].contains(&state.as_str())
            && terminal.is_none()
            && (!self.idle_boundary_passed || !active.is_empty())
        {
            return;
        }
        if self.obligations.flush().is_err() {
            self.ledger.custody_write_failed();
            return;
        }
        let activity = self.activity.project(&state);
        let append = self.options.append.clone();
        self.ledger.update_with(
            LedgerUpdate {
                state,
                waits,
                holds,
                failure: facts.failure,
                last_stop: last_stop.map(|stop| serde_json::to_value(stop).expect("last stop")),
                activity,
            },
            |r| append(r),
        );
    }
    /// Derived only from the current run's own end, bound when its agent_end was emitted: an end processed after a
    /// later run started sets nothing, and so does a fact refresh that carries no bound turn end, even when the goal
    /// collector still returns the previous run's end. An unchanged stop keeps its first `at`.
    fn last_stop(&self, input: Option<&Value>, stalled: bool) -> Option<LifecycleStop> {
        let existing = self
            .ledger
            .current
            .get("lastStop")
            .and_then(|stop| serde_json::from_value::<LifecycleStop>(stop.clone()).ok());
        let Some(run_id) = self.ledger.run_id.clone() else {
            return existing;
        };
        if Some(&run_id) == self.restored_run_id.as_ref()
            || self.stop_facts.as_ref().and_then(|f| f.run_id.as_ref()) != Some(&run_id)
        {
            return existing;
        }
        let deadline = self
            .deadline_stop
            .as_ref()
            .filter(|d| input.is_some() && d.run_id == run_id);
        let Some(detail) = stop_detail(input, self.stop_facts.as_ref(), deadline, stalled) else {
            return existing;
        };
        if let Some(existing) = existing.filter(|stop| same_stop(stop, &detail)) {
            return Some(existing);
        }
        Some(LifecycleStop {
            cause: detail.cause,
            run_id,
            at: detail.at.unwrap_or_else(now_iso),
            epoch: self.ledger.current["epoch"].as_u64().unwrap_or(0),
            deadline: detail.deadline,
            guard: detail.guard,
            todo: detail.todo,
            abort_source: detail.abort_source,
            persisted: detail.persisted,
        })
    }
}
pub struct TurnFacts {
    pub background: Vec<Value>,
    pub synthetic: Vec<(String, String)>,
    pub running: bool,
    pub failure: Option<Value>,
}
/// Shared effective-wake evaluation, used by both the durable controller and AgentSession.
pub fn evaluate_turn_facts(
    input: Option<&Value>,
    goal: Option<&Value>,
    mut background: Vec<Value>,
    counts: (usize, usize),
    run: (bool, bool),
    snapshot: &Value,
    activity: bool,
) -> TurnFacts {
    let (pending, children) = counts;
    let (run_active, interrupted) = run;
    let mut continuation = input.is_some_and(|i| {
        i["willRetry"] == true || i.get("continuationQueued").is_some_and(|v| !v.is_null())
    });
    let mut question = false;
    if let Some(input) = input {
        for wake in input["expectations"].as_array().into_iter().flatten() {
            if (wake["kind"] == "question-to-user" && input["endedBy"] != "clean")
                || (input["deadlineStop"] == true
                    && matches!(
                        wake["kind"].as_str(),
                        Some("armed-timer" | "recovery-staged")
                    ))
            {
                continue;
            }
            match wake["kind"].as_str() {
                Some("pending-messages") => continuation |= pending > 0,
                Some("user-input-queued") => continuation = true,
                Some("armed-timer") if wake["timer"] == "userGrace" => continuation = true,
                Some("armed-timer") => {
                    let source = if wake["timer"] == "providerRetry" {
                        "provider-retry"
                    } else {
                        "goal-monitor"
                    };
                    let mut b = json!({"source":source,"activeCount":1});
                    if source == "provider-retry"
                        && let Some(at) = input.get("providerRetryAtMs")
                    {
                        b["retryAtMs"] = at.clone();
                    }
                    background.push(b);
                }
                Some("recovery-staged") => {
                    background.push(json!({"source":"staged-recovery","activeCount":1}))
                }
                Some("wake-source")
                    if !background.iter().any(|b| b["source"] == wake["source"]) =>
                {
                    background.push(json!({"source":wake["source"],"activeCount":1}));
                }
                Some("question-to-user") => question = true,
                _ => {}
            }
        }
    }
    let mut synthetic = Vec::new();
    let no_wake = children == 0 && background.is_empty() && !continuation && pending == 0;
    if snapshot["role"] == "root" {
        if question {
            synthetic.push((format!("question:{}", snapshot["epoch"]), "question".into()));
        }
        if let Some(g) = goal {
            if g["status"] == "blocked" {
                synthetic.push((
                    format!("goal:{}", g["id"].as_str().unwrap_or_default()),
                    "goal_blocked".into(),
                ));
            } else if g["status"] == "paused" || g.get("continuationStoppedAt").is_some() {
                synthetic.push((
                    format!("goal:{}", g["id"].as_str().unwrap_or_default()),
                    "goal_paused".into(),
                ));
            }
        }
        if interrupted {
            synthetic.push((
                format!("interrupted:{}", snapshot["epoch"]),
                "interrupted_run".into(),
            ));
        }
        let stalled = input.is_some_and(|i| {
            !["provider-error", "policy-rejection"]
                .contains(&i["endedBy"].as_str().unwrap_or_default())
                && (i["deadlineStop"] == true
                    || i["budgetSpent"] == true
                    || ["unknown-abort", "system-abort", "provider-abort"]
                        .contains(&i["endedBy"].as_str().unwrap_or_default())
                    || (i["openWork"]["kind"].as_str().is_some_and(|k| k != "none")
                        && i["goalStopped"] != true))
        });
        if no_wake
            && (stalled
                || goal.is_some_and(|g| {
                    g["status"] == "active"
                        && g.get("continuationStoppedAt").is_none()
                        && !run_active
                        && !activity
                }))
        {
            synthetic.push((format!("stall:{}", snapshot["epoch"]), "stalled".into()));
        }
    }
    let failure = input
        .filter(|i| {
            ["provider-error", "policy-rejection"]
                .contains(&i["endedBy"].as_str().unwrap_or_default())
                && !continuation
                && background.is_empty()
        })
        .map(|i| {
            let mut f = json!({"reason":"provider_error"});
            if let Some(at) = i.get("providerRetryAtMs") {
                f["retryAtMs"] = at.clone();
            }
            f
        });
    TurnFacts {
        background,
        synthetic,
        running: run_active || activity || pending > 0 || continuation,
        failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Store;
    impl crate::work_obligations::CustodyStore for Store {
        fn entries(&self) -> Vec<Value> {
            vec![]
        }
        fn append(
            &mut self,
            _: &crate::work_custody_record::WorkCustodyRecord,
        ) -> Result<(), String> {
            Ok(())
        }
        fn changed(&mut self) {}
    }
    #[test]
    fn neutral_goal_facts_survive_close_clear_and_drive_waits() {
        let mut controller = SessionLifecycleController::new(SessionLifecycleOptions {
            session_id: "neutral-goal-test".into(),
            spawned_by: None,
            read_runtime: Arc::new(LifecycleRuntimeFacts::default),
            append_child: None,
            entries: vec![],
            append: Arc::new(|_| Ok(())),
            custody: Box::new(Store),
        });
        let producer = maho_ext_api::goal_lifecycle::goal_lifecycle_service("neutral-goal-test");
        controller.restore_complete();
        controller.start_run(None);
        producer.transition(Some(maho_ext_api::goal_lifecycle::LifecycleGoal {
            id: "g".into(),
            status: "blocked".into(),
            continuation_stopped: false,
        }));
        assert_eq!(
            controller.snapshot().state,
            SessionLifecycleState::WaitingHuman
        );
        producer.transition(Some(maho_ext_api::goal_lifecycle::LifecycleGoal {
            id: "g".into(),
            status: "complete".into(),
            continuation_stopped: false,
        }));
        producer.transition(None);
        controller.idle();
        assert_eq!(controller.ledger.current["completion"]["goalId"], "g");
        assert_eq!(controller.snapshot().state, SessionLifecycleState::Done);
        producer
            .turn_end(serde_json::json!({"expectations":[{"kind":"wake-source","source":"test"}]}));
        assert_eq!(
            controller.snapshot().state,
            SessionLifecycleState::WaitingBackground
        );
    }

    fn controller() -> SessionLifecycleController {
        SessionLifecycleController::new(SessionLifecycleOptions {
            session_id: "s".into(),
            spawned_by: None,
            read_runtime: Arc::new(LifecycleRuntimeFacts::default),
            append_child: None,
            entries: vec![],
            append: Arc::new(|_| Ok(())),
            custody: Box::new(Store),
        })
    }
    #[test]
    fn ledger_owns_epoch_and_human_revision() {
        let mut c = controller();
        c.restore_complete();
        c.human_input("now");
        c.start_run(None);
        assert_eq!(c.snapshot().human_input_revision, 1);
        let epoch = c.snapshot().epoch;
        c.idle();
        c.start_run(None);
        assert_eq!(c.snapshot().epoch, epoch + 1);
    }
    #[test]
    fn custody_prevents_terminal_and_preserves_background_wait() {
        let mut c = controller();
        c.restore_complete();
        c.start_run(None);
        c.obligations.open(
            "pending",
            crate::work_custody_record::ObligationKind::Background,
            None,
        );
        c.idle();
        assert_eq!(c.snapshot().state, SessionLifecycleState::WaitingBackground);
    }
    fn recording_controller(
        records: Arc<std::sync::Mutex<Vec<Value>>>,
    ) -> SessionLifecycleController {
        SessionLifecycleController::new(SessionLifecycleOptions {
            session_id: "s".into(),
            spawned_by: None,
            read_runtime: Arc::new(LifecycleRuntimeFacts::default),
            append_child: None,
            entries: vec![],
            append: Arc::new(move |r| {
                records.lock().expect("records").push(r.clone());
                Ok(())
            }),
            custody: Box::new(Store),
        })
    }
    #[test]
    fn the_oldest_top_level_tool_is_the_live_activity_and_is_never_written() {
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut c = recording_controller(records.clone());
        c.restore_complete();
        c.start_run(None);
        let tool =
            |id: &str, parent: Option<&str>| maho_agent::types::AgentEvent::ToolExecutionStart {
                tool_call_id: id.into(),
                tool_name: "bash".into(),
                args: Value::Null,
                parent_tool_call_id: parent.map(str::to_owned),
            };
        let written = records.lock().expect("records").len();
        c.observe_activity(&tool("a", None));
        c.observe_activity(&tool("b", None));
        c.observe_activity(&tool("nested", Some("a")));
        let activity = c.ledger.snapshot()["activity"].clone();
        assert_eq!(activity["toolCallId"], "a");
        assert_eq!(activity["outstanding"], 2);
        assert_eq!(records.lock().expect("records").len(), written);
        assert!(c.ledger.snapshot()["activityRevision"]["revision"].as_u64() > Some(0));
        c.idle();
        assert!(c.ledger.snapshot().get("activity").is_none());
        assert!(
            records
                .lock()
                .expect("records")
                .iter()
                .all(|r| r.get("activity").is_none())
        );
    }
    // senpi 034eda3565 (senpi#3864): only the provider stream start records model activity.
    #[test]
    fn model_since_is_recorded_only_at_the_assistant_streaming_start() {
        use maho_agent::types::{AgentEvent, AgentMessage};
        use maho_ai::types::{Message, UserContent, UserMessage};
        let mut c = controller();
        c.restore_complete();
        c.start_run(None);
        let assistant = |text: &str| {
            AgentMessage::from(Message::Assistant(Box::new(
                maho_ai::providers::faux::faux_assistant_message(text, Default::default()),
            )))
        };
        let user = AgentMessage::from(Message::User(UserMessage {
            content: UserContent::Text("Hi".into()),
            timestamp: 1,
        }));
        c.observe_activity(&AgentEvent::MessageStart {
            message: user,
            streaming: Some(true),
        });
        c.observe_activity(&AgentEvent::MessageStart {
            message: assistant("Prepared, not streaming"),
            streaming: None,
        });
        assert!(c.ledger.snapshot().get("activity").is_none());
        let message = assistant("Streaming");
        c.observe_activity(&AgentEvent::MessageStart {
            message: message.clone(),
            streaming: Some(true),
        });
        assert_eq!(c.ledger.snapshot()["activity"]["kind"], "model");
        c.observe_activity(&AgentEvent::MessageEnd { message });
        assert!(c.ledger.snapshot().get("activity").is_none());
    }
    #[test]
    fn an_admitted_run_writes_one_opening_record() {
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut c = recording_controller(records.clone());
        c.restore_complete();
        let before = records.lock().expect("records").len();
        c.human_input("now");
        c.note_run_admitted();
        c.admission_settled();
        assert_eq!(records.lock().expect("records").len(), before);
        c.start_run(None);
        let opened = records.lock().expect("records").len() - before;
        assert_eq!(opened, 1);
        assert_eq!(c.snapshot().human_input_revision, 1);
    }
    #[test]
    fn the_runs_own_abort_becomes_its_last_stop_and_a_new_run_clears_it() {
        let mut c = controller();
        c.restore_complete();
        c.start_run(None);
        let run_id = c.ledger.run_id.clone();
        c.goal_facts_with_stop(
            None,
            Some(json!({"endedBy":"user-abort","expectations":[]})),
            Some(RunStopFacts {
                run_id: run_id.clone(),
                ..Default::default()
            }),
        );
        c.idle();
        let stop = c.snapshot().last_stop.expect("last stop");
        assert_eq!(stop.cause, LifecycleStopCause::Aborted);
        assert_eq!(stop.abort_source, Some(LifecycleAbortSource::User));
        assert_eq!(Some(stop.run_id), run_id);
        c.start_run(None);
        assert!(c.snapshot().last_stop.is_none());
        // A refresh carrying a stale run's facts sets nothing.
        c.goal_facts_with_stop(
            None,
            Some(json!({"endedBy":"user-abort","expectations":[]})),
            Some(RunStopFacts {
                run_id,
                ..Default::default()
            }),
        );
        assert!(c.snapshot().last_stop.is_none());
    }
    #[test]
    fn a_deadline_captured_for_the_run_explains_its_stop() {
        let mut c = controller();
        c.restore_complete();
        c.start_run(None);
        c.deadline_stopped(LifecycleDeadlineKind::Inactivity, 0);
        c.deadline_stop_write_failed();
        c.goal_facts_with_stop(
            None,
            Some(json!({"endedBy":"system-abort","deadlineStop":true,"expectations":[]})),
            Some(RunStopFacts {
                run_id: c.ledger.run_id.clone(),
                ..Default::default()
            }),
        );
        c.idle();
        let stop = c.snapshot().last_stop.expect("last stop");
        assert_eq!(stop.cause, LifecycleStopCause::Deadline);
        assert_eq!(stop.persisted, Some(false));
        assert_eq!(stop.at, "1970-01-01T00:00:00.000Z");
    }
    struct ConsumedStore;
    impl crate::work_obligations::CustodyStore for ConsumedStore {
        fn entries(&self) -> Vec<Value> {
            vec![json!({"type":"custom","customType":"work-custody","data":{
                "id":"w1","key":"delivery:t1","kind":"background","state":"consumed","ticketId":"t1","sessionId":"s"
            }})]
        }
        fn append(
            &mut self,
            _: &crate::work_custody_record::WorkCustodyRecord,
        ) -> Result<(), String> {
            Ok(())
        }
        fn changed(&mut self) {}
    }
    #[test]
    fn consumed_custody_holds_the_reopened_session_for_a_person() {
        let mut c = controller();
        c.mark_interrupted_from_custody(|_| true);
        assert_eq!(
            c.opening_hold_key(),
            None,
            "no ticketed custody, nothing interrupted"
        );
        let mut c = SessionLifecycleController::new(SessionLifecycleOptions {
            session_id: "s".into(),
            spawned_by: None,
            read_runtime: Arc::new(LifecycleRuntimeFacts::default),
            append_child: None,
            entries: vec![],
            append: Arc::new(|_| Ok(())),
            custody: Box::new(ConsumedStore),
        });
        assert_eq!(c.obligations.active().len(), 1);
        c.mark_interrupted_from_custody(|_| false);
        assert_eq!(c.opening_hold_key(), Some(INTERRUPTED_RUN_HOLD_KEY));
        c.human_input("now");
        assert_eq!(c.opening_hold_key(), None);
    }
    #[test]
    fn retries_and_queued_human_input_are_not_stalls() {
        for wake in [
            json!({"kind":"user-input-queued"}),
            json!({"kind":"armed-timer","timer":"providerRetry"}),
        ] {
            let input = json!({"endedBy":"provider-error","expectations":[wake]});
            let f = evaluate_turn_facts(
                Some(&input),
                None,
                vec![],
                (0, 0),
                (false, false),
                &json!({"role":"root","epoch":1}),
                false,
            );
            assert!(f.failure.is_none());
            assert!(f.synthetic.is_empty());
            assert!(f.running || !f.background.is_empty());
        }
    }
    #[test]
    fn exhausted_provider_failure_is_terminal_fact() {
        let f = evaluate_turn_facts(
            Some(&json!({"endedBy":"provider-error","expectations":[]})),
            None,
            vec![],
            (0, 0),
            (false, false),
            &json!({"role":"root","epoch":1}),
            false,
        );
        assert_eq!(f.failure.expect("failure")["reason"], "provider_error");
    }
}
