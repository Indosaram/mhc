//! Atomic external admissions and input-preparation holds.
use crate::extensions::session_control_types::{
    SESSION_CONTROL_DELIVERY_TYPE, is_session_control_delivery_details,
};
use indexmap::{IndexMap, IndexSet};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct InputState {
    next: u64,
    holds: IndexMap<u64, bool>,
    listeners: Vec<Arc<dyn Fn() + Send + Sync>>,
}
#[derive(Clone, Default)]
pub struct ExternalAdmission {
    input: Arc<Mutex<InputState>>,
    ledger: Arc<Mutex<AdmissionLedger>>,
}
#[derive(Default)]
struct AdmissionLedger {
    epoch: u64,
    pending: IndexMap<String, String>,
    emitted: IndexSet<String>,
    failed: IndexMap<String, String>,
    file_takes_writes: Option<bool>,
    closed: Option<String>,
}
pub struct InputHold {
    state: Arc<Mutex<InputState>>,
    id: u64,
    parents: Vec<u64>,
}
impl InputHold {
    pub fn accepted(&self) {
        self.finish(true);
    }
    pub fn end(&self) {
        self.finish(false);
    }
    fn finish(&self, accepted: bool) {
        let listeners = {
            let mut s = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut changed = s.holds.shift_remove(&self.id).is_some();
            if accepted {
                for p in &self.parents {
                    changed |= s.holds.shift_remove(p).is_some();
                }
            }
            if changed && s.holds.is_empty() {
                s.listeners.clone()
            } else {
                vec![]
            }
        };
        for l in listeners {
            l();
        }
    }
}
impl Drop for InputHold {
    fn drop(&mut self) {
        self.end();
    }
}
impl ExternalAdmission {
    pub fn has_input_work(&self) -> bool {
        !self
            .input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .holds
            .is_empty()
    }
    pub fn begin_input(&self, command: bool, submitted_by_command: bool) -> InputHold {
        let mut s = self
            .input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let parents = if submitted_by_command {
            s.holds
                .iter()
                .filter_map(|(id, c)| c.then_some(*id))
                .collect()
        } else {
            vec![]
        };
        s.next += 1;
        let id = s.next;
        s.holds.insert(id, command);
        InputHold {
            state: self.input.clone(),
            id,
            parents,
        }
    }
    pub fn on_inputs_settled(&self, listener: Arc<dyn Fn() + Send + Sync>) {
        self.input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .listeners
            .push(listener);
    }
    pub fn begin_turn(&self) {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epoch += 1;
    }
    pub fn turn_epoch(&self) -> u64 {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .epoch
    }
    pub fn close(&self, reason: String) {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed
            .get_or_insert(reason);
    }
    pub fn reopen(&self) {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .closed = None;
    }
    pub fn gate(&self, editor: &Value) -> Value {
        let held = self.has_input_work();
        let mut g = json!({"can_admit":!held&&editor.get("hold_reason").is_none(),"editor_revision":editor.get("revision").cloned().unwrap_or(json!(0)),"turn_epoch":self.turn_epoch()});
        if held {
            g["hold_reason"] = json!("draft");
        } else if let Some(reason) = editor.get("hold_reason") {
            g["hold_reason"] = reason.clone();
        }
        g
    }
    pub fn admit(
        &self,
        input: &Value,
        editor: &Value,
        busy: bool,
        deliver: impl FnOnce(Value, &str) -> Result<(), String>,
    ) -> Result<Value, String> {
        let gate = self.gate(editor);
        let mut l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(reason) = &l.closed {
            return Err(reason.clone());
        }
        let id = input["delivery_id"].as_str().ok_or("missing delivery id")?;
        let result = |kind: &str, epoch: u64| json!({"kind":kind,"turn_epoch":epoch});
        if l.pending.contains_key(id) || l.emitted.contains(id) || l.failed.contains_key(id) {
            return Ok(result("already_admitted", l.epoch));
        }
        if gate["can_admit"] != true {
            return Ok(result("held_draft", l.epoch));
        }
        if input
            .get("expected_turn_id")
            .is_some_and(|e| e.as_u64() != Some(l.epoch))
        {
            return Ok(result("turn_conflict", l.epoch));
        }
        let busy = busy || l.pending.values().any(|p| p == "start");
        let (kind, lane) = if !busy && input["whenIdle"] != "queue" {
            ("started", "start")
        } else if busy && input["deliverAs"] == "steer" {
            if input.get("expected_turn_id").is_none() {
                return Ok(result("turn_conflict", l.epoch));
            }
            ("steered", "steer")
        } else {
            (
                "queued",
                if busy {
                    "followUp"
                } else {
                    input["deliverAs"].as_str().unwrap_or("followUp")
                },
            )
        };
        let message = delivery_message(input);
        l.pending.insert(id.into(), lane.into());
        let epoch = l.epoch;
        drop(l);
        deliver(message, lane)?;
        Ok(result(kind, epoch))
    }
    pub fn list(&self) -> Value {
        let l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut r = json!({"pending":l.pending.keys().collect::<Vec<_>>(),"emitted":l.emitted.iter().collect::<Vec<_>>()});
        if !l.failed.is_empty() {
            r["failed"] = json!(
                l.failed
                    .iter()
                    .map(|(id, e)| json!({"delivery_id":id,"error":e}))
                    .collect::<Vec<_>>()
            );
        }
        r
    }
    pub fn observe_persisted(&self, message: &Value) {
        let mut l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        l.file_takes_writes = Some(true);
        if let Some(id) = delivery_id_of(message) {
            l.pending.shift_remove(id);
            l.emitted.insert(id.into());
        }
    }
    pub fn observe_refused(&self, message: &Value, error: String) {
        let mut l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        l.file_takes_writes = Some(false);
        if let Some(id) = delivery_id_of(message)
            && !l.emitted.contains(id) {
                l.pending.shift_remove(id);
                l.failed.insert(id.into(), error);
            }
    }
    pub fn observe_run_settled(&self) {
        let mut l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if l.file_takes_writes != Some(false) {
            l.failed.clear();
        }
        for lane in l.pending.values_mut() {
            if lane == "start" {
                *lane = "followUp".into();
            }
        }
    }
    pub fn drop_queued(&self) {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .retain(|_, lane| lane == "start");
    }
}
pub fn delivery_message(input: &Value) -> Value {
    let mut details = json!({"delivery_id":input["delivery_id"],"source":"session_control","deliverAs":input["deliverAs"]});
    for k in ["sender", "display_text"] {
        if let Some(v) = input.get(k) {
            details[k] = v.clone();
        }
    }
    json!({"role":"custom","customType":SESSION_CONTROL_DELIVERY_TYPE,"content":input["text"],"display":true,"details":details,"timestamp":chrono::Utc::now().timestamp_millis()})
}
pub fn delivery_id_of(message: &Value) -> Option<&str> {
    (message["role"] == "custom"
        && message["customType"] == SESSION_CONTROL_DELIVERY_TYPE
        && is_session_control_delivery_details(&message["details"]))
    .then(|| message["details"]["delivery_id"].as_str())
    .flatten()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_command_child_releases_parents_once() {
        let a = ExternalAdmission::default();
        let parent = a.begin_input(true, false);
        let child = a.begin_input(false, true);
        assert!(a.has_input_work());
        child.accepted();
        assert!(!a.has_input_work());
        parent.end();
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;
    #[test]
    fn input_holds_fence_delivery_and_epoch_conflicts_do_not_enqueue() {
        let admission = ExternalAdmission::default();
        let hold = admission.begin_input(false, false);
        let input = json!({"delivery_id":"d","text":"hi","deliverAs":"steer"});
        assert_eq!(
            admission
                .admit(&input, &json!({}), true, |_, _| panic!("held"))
                .expect("gate")["kind"],
            "held_draft"
        );
        hold.accepted();
        admission.begin_turn();
        assert_eq!(
            admission
                .admit(&input, &json!({}), true, |_, _| panic!("epoch"))
                .expect("gate")["kind"],
            "turn_conflict"
        );
        let mut input = input;
        input["expected_turn_id"] = json!(1);
        assert_eq!(
            admission
                .admit(&input, &json!({}), true, |_, lane| {
                    assert_eq!(lane, "steer");
                    Ok(())
                })
                .expect("admit")["kind"],
            "steered"
        );
        assert_eq!(
            admission
                .admit(&input, &json!({}), true, |_, _| panic!("duplicate"))
                .expect("dedupe")["kind"],
            "already_admitted"
        );
        admission.observe_persisted(&delivery_message(&input));
        assert_eq!(admission.list()["emitted"], json!(["d"]));
    }
}
