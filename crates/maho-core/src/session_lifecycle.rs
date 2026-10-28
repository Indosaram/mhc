//! Durable session lifecycle controller, shared by session front ends.
use crate::lifecycle_children::LifecycleChildren;
use crate::lifecycle_ledger::LifecycleLedger;
use crate::session_activity::SessionActivitySnapshot;
use crate::session_lifecycle_types::*;
use serde_json::{Value, json};
use std::sync::Arc;

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
    goal: Option<Value>,
    liveness: Option<Value>,
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
            goal: None,
            liveness,
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
    pub fn admission_settled(&mut self) {
        self.admitting_human = false;
        if !self.ledger.run_active
            && !self.ledger.interrupted
            && (self.options.read_runtime)().pending_messages == 0
        {
            self.pending_human = false;
        }
        self.recompute();
    }
    pub fn start_run(&mut self, origin: Option<EpochOrigin>) {
        self.admitting_human = false;
        self.idle_boundary_passed = false;
        self.liveness = None;
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
        self.checkpoint();
    }
    pub fn idle(&mut self) {
        self.ledger.run_active = false;
        self.idle_boundary_passed = true;
        if let Some(id) = self.ledger.run_id.clone() {
            self.obligations.settle_run(&id);
        }
        self.recompute();
        self.checkpoint();
    }
    pub fn goal_facts(&mut self, goal: Option<Value>, input: Option<Value>) {
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
                && let Some(id) = g["id"].as_str() {
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
        let children = reports
            .iter()
            .filter(|r| {
                r["childSessionId"] != self.options.session_id && r.get("terminal").is_none()
            })
            .count()
            .max(
                active
                    .iter()
                    .filter(|r| r.kind == crate::work_custody_record::ObligationKind::Children)
                    .map(|r| r.active_count.unwrap_or(1.) as usize)
                    .sum(),
            );
        let background=active.iter().filter(|r|r.kind==crate::work_custody_record::ObligationKind::Background).map(|r|json!({"source":r.source.as_deref().unwrap_or(&r.key),"activeCount":r.active_count.unwrap_or(1.) as u64})).collect();
        let facts = evaluate_turn_facts(
            self.liveness.as_ref(),
            self.goal.as_ref(),
            background, (runtime.pending_messages,
            children), (self.ledger.run_active,
            self.ledger.interrupted),
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
        let waits = json!({"children":children,"background":facts.background,"human":human});
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
        let append = self.options.append.clone();
        self.ledger
            .update(&state, waits, holds, facts.failure, |r| append(r));
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
                        && let Some(at) = input.get("providerRetryAtMs") {
                            b["retryAtMs"] = at.clone();
                        }
                    background.push(b);
                }
                Some("recovery-staged") => {
                    background.push(json!({"source":"staged-recovery","activeCount":1}))
                }
                Some("wake-source")
                    if !background.iter().any(|b| b["source"] == wake["source"]) => {
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
                vec![], (0,
                0), (false,
                false),
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
            vec![], (0,
            0), (false,
            false),
            &json!({"role":"root","epoch":1}),
            false,
        );
        assert_eq!(f.failure.expect("failure")["reason"], "provider_error");
    }
}
