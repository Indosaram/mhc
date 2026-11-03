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
/// senpi `onEmitted` listener: told the delivery id whose transcript entry was just written.
pub type EmittedListener = Arc<dyn Fn(&str) + Send + Sync>;
#[derive(Clone, Default)]
pub struct ExternalAdmission {
    input: Arc<Mutex<InputState>>,
    ledger: Arc<Mutex<AdmissionLedger>>,
    emitted_listeners: Arc<Mutex<Vec<(u64, EmittedListener)>>>,
    next_emitted_listener: Arc<std::sync::atomic::AtomicU64>,
}
/// Removes an `on_emitted` listener when dropped.
pub struct EmittedSubscription {
    listeners: std::sync::Weak<Mutex<Vec<(u64, EmittedListener)>>>,
    id: u64,
}
impl Drop for EmittedSubscription {
    fn drop(&mut self) {
        if let Some(listeners) = self.listeners.upgrade() {
            listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|(id, _)| *id != self.id);
        }
    }
}
/// A durable-custody reservation (senpi `PendingLane` function): held while the restored delivery is still owned.
pub type RestoredOwnership = Arc<dyn Fn() -> bool + Send + Sync>;
#[derive(Default)]
struct AdmissionLedger {
    epoch: u64,
    pending: IndexMap<String, String>,
    /// Restored queued deliveries custody will replay, reserved before recovery or gateway admission (senpi#11209).
    restored: IndexMap<String, RestoredOwnership>,
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
impl AdmissionLedger {
    fn holds(&self, id: &str) -> bool {
        self.pending.contains_key(id)
            || self.restored.contains_key(id)
            || self.emitted.contains(id)
            || self.failed.contains_key(id)
    }
}
impl ExternalAdmission {
    /// Releases restored reservations no longer owned. The ownership closures read custody (session manager,
    /// work obligations), so they run after the ledger lock is released, never under it.
    fn release_restored(&self) {
        let restored: Vec<(String, RestoredOwnership)> = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .restored
            .iter()
            .map(|(id, owned)| (id.clone(), owned.clone()))
            .collect();
        let released: Vec<String> = restored
            .into_iter()
            .filter(|(_, owned)| !owned())
            .map(|(id, _)| id)
            .collect();
        if released.is_empty() {
            return;
        }
        let mut l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for id in &released {
            l.restored.shift_remove(id);
        }
    }
    /// senpi `restoreQueued`: durable custody will replay this delivery; reserve its id before asynchronous recovery
    /// or gateway admission. Recovery chooses the payload's queue lane; this only tracks durable ownership.
    pub fn restore_queued(&self, delivery_id: &str, is_owned: RestoredOwnership) {
        let mut l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if l.holds(delivery_id) {
            return;
        }
        l.restored.insert(delivery_id.to_owned(), is_owned);
    }
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
        self.release_restored();
        let mut l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(reason) = &l.closed {
            return Err(reason.clone());
        }
        let id = input["delivery_id"].as_str().ok_or("missing delivery id")?;
        let result = |kind: &str, epoch: u64| json!({"kind":kind,"turn_epoch":epoch});
        if l.holds(id) {
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
        self.release_restored();
        let l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending: Vec<&String> = l.pending.keys().chain(l.restored.keys()).collect();
        let mut r = json!({"pending":pending,"emitted":l.emitted.iter().collect::<Vec<_>>()});
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
    /// senpi `onEmitted`: an admitted delivery's own transcript entry was written (once per delivery).
    pub fn on_emitted(&self, listener: EmittedListener) -> EmittedSubscription {
        let id = self
            .next_emitted_listener
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.emitted_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, listener));
        EmittedSubscription {
            listeners: Arc::downgrade(&self.emitted_listeners),
            id,
        }
    }
    pub fn observe_persisted(&self, message: &Value) {
        let emitted = {
            let mut l = self
                .ledger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            l.file_takes_writes = Some(true);
            match delivery_id_of(message) {
                Some(id) if !l.emitted.contains(id) => {
                    l.pending.shift_remove(id);
                    l.restored.shift_remove(id);
                    l.emitted.insert(id.into());
                    Some(id.to_owned())
                }
                _ => None,
            }
        };
        // Listeners run with no ledger lock held, so one may read the ledger (senpi emits after the add).
        if let Some(id) = emitted {
            let listeners: Vec<EmittedListener> = self
                .emitted_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .map(|(_, listener)| Arc::clone(listener))
                .collect();
            for listener in listeners {
                listener(&id);
            }
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
                l.restored.shift_remove(id);
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
    /// Cleared carriers are no longer held; custody not yet enqueued for recovery still owns its delivery.
    pub fn drop_queued(&self) {
        self.release_restored();
        let mut l = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        l.pending.retain(|_, lane| lane == "start");
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
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let recorded = Arc::clone(&seen);
        let subscription = admission.on_emitted(Arc::new(move |id| {
            recorded.lock().unwrap().push(id.to_owned())
        }));
        admission.observe_persisted(&delivery_message(&input));
        assert_eq!(admission.list()["emitted"], json!(["d"]));
        // senpi `observePersisted`: a delivery is emitted once; its listeners hear it once.
        admission.observe_persisted(&delivery_message(&input));
        drop(subscription);
        let other = json!({"delivery_id":"e","deliverAs":"steer"});
        admission.observe_persisted(&delivery_message(&other));
        assert_eq!(admission.list()["emitted"], json!(["d", "e"]));
        assert_eq!(*seen.lock().unwrap(), vec!["d".to_owned()]);
    }
}

#[cfg(test)]
mod restored_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// senpi test/suite/regressions/11209-restored-external-delivery.test.ts and
    /// 11209-restored-reservation-queue-clear.test.ts: a restored delivery keeps its reservation across a queue
    /// clear while owned, and is released once custody no longer owns it.
    #[test]
    fn a_restored_delivery_is_reserved_until_custody_releases_it() {
        let admission = ExternalAdmission::default();
        let owned = Arc::new(AtomicBool::new(true));
        let reader = owned.clone();
        admission.restore_queued("d", Arc::new(move || reader.load(Ordering::SeqCst)));
        let input = json!({"delivery_id":"d","text":"hi","deliverAs":"followUp"});
        assert_eq!(
            admission
                .admit(&input, &json!({}), false, |_, _| panic!("reserved"))
                .expect("dedupe")["kind"],
            "already_admitted"
        );
        admission.drop_queued();
        assert_eq!(admission.list()["pending"], json!(["d"]));
        owned.store(false, Ordering::SeqCst);
        assert_eq!(admission.list()["pending"], json!([]));
        let mut delivered = false;
        admission
            .admit(&input, &json!({}), false, |_, _| {
                delivered = true;
                Ok(())
            })
            .expect("admit");
        assert!(delivered);
    }

    /// The ownership check runs outside the ledger lock, so a closure that reads the admission itself (as
    /// custody reads do through the session) cannot self-deadlock.
    #[test]
    fn an_ownership_check_runs_outside_the_ledger_lock() {
        let admission = ExternalAdmission::default();
        let reader = admission.clone();
        admission.restore_queued("d", Arc::new(move || reader.turn_epoch() == 0));
        assert_eq!(admission.list()["pending"], json!(["d"]));
        admission.begin_turn();
        admission.drop_queued();
        assert_eq!(admission.list()["pending"], json!([]));
    }
}
