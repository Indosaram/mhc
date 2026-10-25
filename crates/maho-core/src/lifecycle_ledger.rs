//! Lifecycle epoch/revision ordering and durable self-settlement fencing.
use crate::lifecycle_attention::LifecycleAttention;
use crate::lifecycle_record::read_lifecycle_record;
use serde_json::{Value, json};
pub struct LifecycleLedger {
    pub current: Value,
    pub attention: LifecycleAttention,
    session_id: String,
    sealed: bool,
    request: Option<Value>,
    goal_completed: Option<String>,
    run_completed: Option<String>,
    admission_write_failed: bool,
    pub run_id: Option<String>,
    pub run_active: bool,
    pub interrupted: bool,
    pub last_turn_end: Option<Value>,
}
impl LifecycleLedger {
    pub fn new(session_id: &str, spawned_by: Option<&Value>, entries: &[Value]) -> Self {
        let restored = entries
            .iter()
            .filter(|e| e["type"] == "custom" && e["customType"] == "session-lifecycle")
            .filter_map(|e| read_lifecycle_record(&e["data"]))
            .filter(|r| r["sessionId"] == session_id)
            .max_by(|a, b| {
                a["seq"]
                    .as_f64()
                    .partial_cmp(&b["seq"].as_f64())
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        let mut current=restored.clone().unwrap_or_else(||json!({"state":"done","seq":0,"epoch":0,"epochOrigin":"automatic","humanInputRevision":0,"selfSettleEligible":true,"restoring":true,"persisted":false,"runPaused":false,"since":chrono::Utc::now().to_rfc3339(),"role":if spawned_by.is_some(){"child"}else{"root"},"rootSessionId":spawned_by.map(|s|s["rootSessionId"].clone()).unwrap_or(json!(session_id))}));
        if restored.is_none()
            && let Some(s) = spawned_by {
                current["parentSessionId"] = s["parentSessionId"].clone();
                current["taskId"] = s["taskId"].clone();
            }
        let r = restored.as_ref().unwrap_or(&Value::Null);
        let sealed = r["epochSealed"] == true;
        let run_id = r["runId"].as_str().map(str::to_owned);
        let interrupted = r["interrupted"] == true || (r["runActive"] == true && !sealed);
        let goal_completed = r["completedGoalId"].as_str().map(str::to_owned);
        let run_completed = r["completedRunId"].as_str().map(str::to_owned);
        let last_turn_end = r.get("lastTurnEnd").cloned();
        if r.get("settleRequest").is_some() && !sealed {
            current["selfSettleEligible"] = json!(false);
        }
        for k in [
            "sessionId",
            "attentionSeq",
            "synthetic",
            "epochSealed",
            "settleRequest",
            "runId",
            "runActive",
            "interrupted",
            "completedGoalId",
            "completedRunId",
            "lastTurnEnd",
        ] {
            current.as_object_mut().expect("lifecycle object").remove(k);
        }
        Self {
            current,
            attention: LifecycleAttention::new(session_id, restored.as_ref()),
            session_id: session_id.into(),
            sealed,
            request: None,
            goal_completed,
            run_completed,
            admission_write_failed: false,
            run_id,
            run_active: false,
            interrupted,
            last_turn_end,
        }
    }
    pub fn snapshot(&self) -> &Value {
        &self.current
    }
    pub fn custody_write_failed(&mut self) {
        self.current["persisted"] = json!(false);
        self.current["selfSettleEligible"] = json!(false);
    }
    pub fn release_restore_barrier(&mut self) {
        self.current["restoring"] = json!(false);
    }
    pub fn human_input(
        &mut self,
        at: &str,
        append: impl FnOnce(&Value) -> Result<(), String>,
    ) -> bool {
        self.request = None;
        let mut n = self.current.clone();
        n.as_object_mut().expect("object").remove("settleDecision");
        if n["epoch"] == 0
            && ["done", "failed", "cancelled"].contains(&n["state"].as_str().unwrap_or(""))
        {
            n["state"] = json!("running");
            n["since"] = json!(at);
        }
        n["humanInputRevision"] = json!(n["humanInputRevision"].as_u64().unwrap_or(0) + 1);
        let ok = self.write(n, append);
        if !ok {
            self.admission_write_failed = true;
        }
        ok
    }
    pub fn start_run(&mut self, origin: &str) {
        self.last_turn_end = None;
        self.interrupted = false;
        if self.sealed || self.current["epoch"] == 0 {
            self.sealed = false;
            self.request = None;
            self.goal_completed = None;
            self.run_completed = None;
            for k in ["completionId", "completion", "failure", "settleDecision"] {
                self.current.as_object_mut().expect("object").remove(k);
            }
            self.current["epoch"] = json!(self.current["epoch"].as_u64().unwrap_or(0) + 1);
            self.current["epochOrigin"] = json!(origin);
            self.current["selfSettleEligible"] = json!(!self.admission_write_failed);
        }
        self.run_id = Some(uuid::Uuid::new_v4().to_string());
        self.run_active = true;
        self.admission_write_failed = false;
    }
    pub fn mark_goal_completed(
        &mut self,
        id: &str,
        append: impl FnOnce(&Value) -> Result<(), String>,
    ) {
        if self.current["epoch"] == 0 || self.sealed || self.goal_completed.as_deref() == Some(id) {
            return;
        }
        self.goal_completed = Some(id.into());
        self.checkpoint(append);
    }
    pub fn mark_run_completed(
        &mut self,
        id: &str,
        append: impl FnOnce(&Value) -> Result<(), String>,
    ) {
        if self.current["epoch"] == 0 || self.sealed || self.run_completed.as_deref() == Some(id) {
            return;
        }
        self.run_completed = Some(id.into());
        self.checkpoint(append);
    }
    pub fn request_self_settle(
        &mut self,
        append: impl FnOnce(&Value) -> Result<(), String>,
    ) -> Value {
        let mut result = json!({"epoch":self.current["epoch"],"humanInputRevision":self.current["humanInputRevision"],"recorded":false});
        let reason = if self.current["holds"]
            .as_array()
            .is_some_and(|h| !h.is_empty())
        {
            Some("held")
        } else if self.current["epoch"] == 0 || self.sealed {
            Some("no_epoch")
        } else if self.current["selfSettleEligible"] != true || self.current["role"] != "root" {
            Some("not_eligible")
        } else {
            None
        };
        if let Some(reason) = reason {
            result["reason"] = json!(reason);
            return result;
        }
        self.request = Some(
            json!({"epoch":self.current["epoch"],"revision":self.current["humanInputRevision"]}),
        );
        if self.write(self.current.clone(), append) {
            result["recorded"] = json!(true);
        } else {
            self.request = None;
            self.current["selfSettleEligible"] = json!(false);
            result["reason"] = json!("not_eligible");
        }
        result
    }
    pub fn update(
        &mut self,
        state: &str,
        waits: Value,
        holds: Vec<Value>,
        failure: Option<Value>,
        append: impl FnOnce(&Value) -> Result<(), String>,
    ) -> bool {
        if !holds.is_empty() || state == "failed" {
            self.request = None;
        }
        let terminal = state == "done" || state == "failed";
        let mut n = self.current.clone();
        for k in ["completionId", "completion", "failure", "settleDecision"] {
            n.as_object_mut().expect("object").remove(k);
        }
        let now = chrono::Utc::now().to_rfc3339();
        let standing = self.request.as_ref().is_some_and(|r| {
            r["epoch"] == n["epoch"]
                && r["revision"] == n["humanInputRevision"]
                && n["selfSettleEligible"] == true
                && holds.is_empty()
        });
        n["state"] = json!(state);
        n["waitingOn"] = waits;
        n["runPaused"] = json!(holds.iter().any(|h| h["kind"] == "run_fence"));
        n["holds"] = json!(holds);
        if self.current["state"] != state {
            n["since"] = json!(now);
        }
        if terminal && n["epoch"] != 0 {
            n["completionId"] = json!(format!("{}:{}", self.session_id, n["epoch"]));
        }
        if state == "done" && n["epoch"] != 0 {
            n["completion"] = if self.current["state"] == "done"
                && self.sealed
                && self.current.get("completion").is_some()
            {
                self.current["completion"].clone()
            } else if let Some(id) = &self.goal_completed {
                json!({"kind":"goal","at":now,"goalId":id})
            } else if let Some(id) = &self.run_completed {
                json!({"kind":"run","at":now,"runId":id})
            } else {
                json!({"kind":if standing{"agent"}else{"reply"},"at":now})
            };
        }
        if state == "failed"
            && let Some(f) = failure {
                n["failure"] = f;
            }
        if state == "done"
            && (standing || (self.sealed && self.current.get("settleDecision").is_some()))
        {
            n["settleDecision"] = self
                .current
                .get("settleDecision")
                .cloned()
                .unwrap_or_else(|| json!({"decisionId":uuid::Uuid::new_v4().to_string()}));
        }
        if terminal && n["epoch"] != 0 {
            self.sealed = true;
        }
        if self.current["restoring"] == true {
            return false;
        }
        let same = [
            "state",
            "epoch",
            "humanInputRevision",
            "runPaused",
            "selfSettleEligible",
            "waitingOn",
            "holds",
            "failure",
        ]
        .iter()
        .all(|k| self.current[*k] == n[*k]);
        if self.current["persisted"] == true && same {
            return false;
        }
        if terminal && n["epoch"] == 0 {
            self.current = n;
            return false;
        }
        self.write(n, append)
    }
    pub fn checkpoint(&mut self, append: impl FnOnce(&Value) -> Result<(), String>) -> bool {
        self.write(self.current.clone(), append)
    }
    fn write(&mut self, mut n: Value, append: impl FnOnce(&Value) -> Result<(), String>) -> bool {
        n["seq"] = json!(self.current["seq"].as_u64().unwrap_or(0) + 1);
        n["persisted"] = json!(true);
        let mut r = n.clone();
        r["sessionId"] = json!(self.session_id);
        r["attentionSeq"] = json!(self.attention.attention_seq());
        r["synthetic"] = self.attention.synthetic();
        r["epochSealed"] = json!(self.sealed);
        r["runActive"] = json!(self.run_active);
        r["interrupted"] = json!(self.interrupted);
        for (k, v) in [
            ("runId", self.run_id.as_ref()),
            ("completedGoalId", self.goal_completed.as_ref()),
            ("completedRunId", self.run_completed.as_ref()),
        ] {
            if let Some(v) = v {
                r[k] = json!(v);
            }
        }
        if let Some(t) = &self.last_turn_end {
            r["lastTurnEnd"] = t.clone();
        }
        if let Some(q) = &self.request {
            r["settleRequest"] = q.clone();
        }
        let ok = append(&r).is_ok();
        self.current = n;
        if !ok {
            self.custody_write_failed();
        }
        ok
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn human_revision_and_restore_fence_settlement() {
        let mut l = LifecycleLedger::new("s", None, &[]);
        l.release_restore_barrier();
        l.human_input("now", |_| Ok(()));
        l.start_run("human");
        assert_eq!(l.request_self_settle(|_| Ok(()))["recorded"], true);
        l.human_input("later", |_| Ok(()));
        l.update(
            "done",
            json!({"children":0,"human":[],"background":[]}),
            vec![],
            None,
            |_| Ok(()),
        );
        assert_eq!(l.snapshot()["completion"]["kind"], "reply");
        assert!(l.snapshot().get("settleDecision").is_none());
    }
}

#[cfg(test)]
mod terminal_identity_tests {
    use super::*;
    #[test]
    fn child_terminal_identity_is_not_the_roots() {
        for (id, parent) in [
            ("root-1", None),
            (
                "child-1",
                Some(
                    json!({"parentSessionId":"root-1","rootSessionId":"root-1","taskId":"task-1"}),
                ),
            ),
        ] {
            let mut ledger = LifecycleLedger::new(id, parent.as_ref(), &[]);
            ledger.release_restore_barrier();
            ledger.start_run("human");
            ledger.update(
                "done",
                json!({"children":0,"background":[],"human":[]}),
                vec![],
                None,
                |_| Ok(()),
            );
            assert_eq!(ledger.snapshot()["completionId"], format!("{id}:1"));
        }
    }
    #[test]
    fn failed_admission_write_disables_self_settlement() {
        let mut ledger = LifecycleLedger::new("s", None, &[]);
        ledger.release_restore_barrier();
        assert!(!ledger.human_input("now", |_| Err("disk full".into())));
        ledger.start_run("human");
        assert_eq!(
            ledger.request_self_settle(|_| Ok(()))["reason"],
            "not_eligible"
        );
    }
}

#[cfg(test)]
mod restore_tests {
    use super::*;
    #[test]
    fn unsealed_intent_cannot_self_settle_after_restart() {
        let mut ledger = LifecycleLedger::new("s", None, &[]);
        ledger.release_restore_barrier();
        ledger.start_run("human");
        let mut saved = Value::Null;
        assert_eq!(
            ledger.request_self_settle(|r| {
                saved = r.clone();
                Ok(())
            })["recorded"],
            true
        );
        let restored = LifecycleLedger::new(
            "s",
            None,
            &[json!({"type":"custom","customType":"session-lifecycle","data":saved})],
        );
        assert_eq!(restored.snapshot()["selfSettleEligible"], false);
        assert!(restored.interrupted);
    }
}
