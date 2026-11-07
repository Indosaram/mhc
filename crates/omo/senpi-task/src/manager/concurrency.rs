//! Per-model/provider admission custody (`manager/concurrency.ts`).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender},
};

pub type Grant = Box<dyn FnOnce() + Send>;

#[derive(Debug, Clone, Default)]
pub struct TaskConcurrencyConfig {
    pub default_concurrency: Option<usize>,
    pub provider_concurrency: Option<BTreeMap<String, usize>>,
    pub model_concurrency: Option<BTreeMap<String, usize>>,
    pub global_concurrency: Option<usize>,
}

const DEFAULT_LIMIT: usize = 5;
type Limit = Option<usize>;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueuedTaskAdmission {
    pub model: String,
    pub lane_key: String,
    pub task_id: String,
    pub run_epoch: i64,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionPhase {
    PreGrant,
    PostGrant,
    Parked,
}

pub type ParkedLease = Arc<QueuedTaskAdmission>;
struct Waiter {
    admission: QueuedTaskAdmission,
    grant: Grant,
}
struct ParkedEntry {
    lease: ParkedLease,
    resumable: bool,
    resumed: Vec<Sender<()>>,
}
impl ParkedEntry {
    fn settle(&mut self) {
        for sender in self.resumed.drain(..) {
            let _ = sender.send(());
        }
    }
}

#[derive(Default)]
pub struct TaskConcurrency {
    config: TaskConcurrencyConfig,
    counts: HashMap<String, usize>,
    queues: HashMap<String, VecDeque<Waiter>>,
    leases: HashMap<(String, i64), QueuedTaskAdmission>,
    active_global: usize,
    parked: HashMap<(String, i64), ParkedEntry>,
    sequence: u64,
    legacy_epoch: i64,
}
fn provider_of(model: &str) -> &str {
    model.split('/').next().unwrap_or(model)
}
fn own(record: Option<&BTreeMap<String, usize>>, key: &str) -> Option<usize> {
    record.and_then(|record| record.get(key).copied())
}
fn unlimited_if_zero(limit: usize) -> Limit {
    (limit != 0).then_some(limit)
}
fn lease_key(task_id: &str, epoch: i64) -> (String, i64) {
    (task_id.to_string(), epoch)
}

impl TaskConcurrency {
    pub fn new(config: TaskConcurrencyConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }
    pub fn get_limit(&self, model: &str) -> Limit {
        if let Some(limit) = own(self.config.model_concurrency.as_ref(), model) {
            return unlimited_if_zero(limit);
        }
        let provider = provider_of(model);
        if let Some(limit) = own(self.config.provider_concurrency.as_ref(), provider) {
            return unlimited_if_zero(limit);
        }
        // `concurrency-routing.ts:13`: opengateway is implicitly unbounded.
        if provider == "opengateway" {
            return None;
        }
        unlimited_if_zero(self.config.default_concurrency.unwrap_or(DEFAULT_LIMIT))
    }
    pub fn get_key(&self, model: &str) -> String {
        self.key_of(model).to_string()
    }
    fn key_of<'a>(&self, model: &'a str) -> &'a str {
        if own(self.config.model_concurrency.as_ref(), model).is_some() {
            return model;
        }
        let provider = provider_of(model);
        if own(self.config.provider_concurrency.as_ref(), provider).is_some() {
            return provider;
        }
        model
    }
    fn lane_has_room(&self, model: &str, lane: &str) -> bool {
        self.get_limit(model)
            .is_none_or(|limit| self.counts.get(lane).copied().unwrap_or(0) < limit)
    }
    fn global_limit(&self) -> Limit {
        self.config.global_concurrency.and_then(unlimited_if_zero)
    }
    /// `concurrency.ts:259-263 #globalHasRoom`: `undefined || 0` means unlimited.
    fn global_has_room(&self) -> bool {
        self.global_limit()
            .is_none_or(|limit| self.active_global < limit)
    }
    pub fn has_free_slot(&self, model: &str) -> bool {
        let lane = self.key_of(model);
        self.queues.get(lane).is_none_or(VecDeque::is_empty)
            && self.lane_has_room(model, lane)
            && self.global_has_room()
    }
    fn next_legacy_epoch(&mut self) -> i64 {
        self.legacy_epoch -= 1;
        self.legacy_epoch
    }
    fn reserve_sequence(&mut self) -> u64 {
        let sequence = self.sequence;
        self.sequence += 1;
        sequence
    }
    fn record_lease(&mut self, admission: QueuedTaskAdmission) {
        if self.get_limit(&admission.model).is_some() {
            *self.counts.entry(admission.lane_key.clone()).or_default() += 1;
        }
        let inserted = self
            .leases
            .insert(
                lease_key(&admission.task_id, admission.run_epoch),
                admission,
            )
            .is_none();
        if inserted {
            self.active_global += 1;
        }
    }
    fn drop_lease(&mut self, admission: &QueuedTaskAdmission) {
        if self
            .leases
            .remove(&lease_key(&admission.task_id, admission.run_epoch))
            .is_some()
        {
            self.active_global = self.active_global.saturating_sub(1);
        }
        if let Some(count) = self.counts.get_mut(&admission.lane_key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.counts.remove(&admission.lane_key);
            }
        }
    }
    pub fn try_acquire(&mut self, model: &str, task_id: &str, run_epoch: i64) -> bool {
        if self.lease_state(task_id, run_epoch).is_some() || !self.has_free_slot(model) {
            return false;
        }
        let admission = QueuedTaskAdmission {
            model: model.into(),
            lane_key: self.get_key(model),
            task_id: task_id.into(),
            run_epoch,
            sequence: self.reserve_sequence(),
        };
        self.record_lease(admission);
        true
    }
    pub fn acquire(&mut self, model: &str, task_id: &str) {
        let run_epoch = self.next_legacy_epoch();
        let admission = QueuedTaskAdmission {
            model: model.into(),
            lane_key: self.get_key(model),
            task_id: task_id.into(),
            run_epoch,
            sequence: self.reserve_sequence(),
        };
        self.record_lease(admission);
    }
    pub fn enqueue(&mut self, model: &str, task_id: &str, grant: Grant) -> usize {
        let epoch = self.next_legacy_epoch();
        self.enqueue_epoch(model, task_id, epoch, grant)
    }
    pub fn enqueue_epoch(
        &mut self,
        model: &str,
        task_id: &str,
        run_epoch: i64,
        grant: Grant,
    ) -> usize {
        let admission = QueuedTaskAdmission {
            model: model.into(),
            lane_key: self.get_key(model),
            task_id: task_id.into(),
            run_epoch,
            sequence: self.reserve_sequence(),
        };
        self.requeue(admission, grant)
    }
    pub fn requeue(&mut self, admission: QueuedTaskAdmission, grant: Grant) -> usize {
        self.sequence = self.sequence.max(admission.sequence + 1);
        let task_id = admission.task_id.clone();
        let epoch = admission.run_epoch;
        let queue = self.queues.entry(admission.lane_key.clone()).or_default();
        let index = queue
            .iter()
            .position(|w| w.admission.sequence > admission.sequence)
            .unwrap_or(queue.len());
        queue.insert(index, Waiter { admission, grant });
        queue
            .iter()
            .position(|w| w.admission.task_id == task_id && w.admission.run_epoch == epoch)
            .map_or(0, |i| i + 1)
    }
    pub fn queue_position(&self, model: &str, task_id: &str) -> Option<usize> {
        self.queues
            .get(self.key_of(model))?
            .iter()
            .position(|w| w.admission.task_id == task_id)
            .map(|i| i + 1)
    }
    fn take(
        &mut self,
        model: &str,
        task_id: &str,
        epoch: Option<i64>,
    ) -> Option<QueuedTaskAdmission> {
        let lane = self.get_key(model);
        let queue = self.queues.get_mut(&lane)?;
        let index = queue.iter().position(|w| {
            w.admission.task_id == task_id && epoch.is_none_or(|e| w.admission.run_epoch == e)
        })?;
        let admission = queue.remove(index)?.admission;
        if queue.is_empty() {
            self.queues.remove(&lane);
        }
        Some(admission)
    }
    pub fn remove(&mut self, model: &str, task_id: &str) -> bool {
        self.take(model, task_id, None).is_some()
    }
    pub fn withdraw(
        &mut self,
        model: &str,
        task_id: &str,
        epoch: i64,
    ) -> Option<QueuedTaskAdmission> {
        self.take(model, task_id, Some(epoch))
    }
    pub fn admission(
        &self,
        task_id: &str,
        epoch: i64,
    ) -> Option<(QueuedTaskAdmission, AdmissionPhase)> {
        let key = lease_key(task_id, epoch);
        if let Some(lease) = self.leases.get(&key) {
            return Some((lease.clone(), AdmissionPhase::PostGrant));
        }
        if let Some(entry) = self.parked.get(&key) {
            return Some(((*entry.lease).clone(), AdmissionPhase::Parked));
        }
        self.queues
            .values()
            .flat_map(|q| q.iter())
            .find(|w| w.admission.task_id == task_id && w.admission.run_epoch == epoch)
            .map(|w| (w.admission.clone(), AdmissionPhase::PreGrant))
    }
    pub fn lease_state(&self, task_id: &str, epoch: i64) -> Option<&'static str> {
        let key = lease_key(task_id, epoch);
        if self.leases.contains_key(&key) {
            Some("held")
        } else if self.parked.contains_key(&key) {
            Some("parked")
        } else {
            None
        }
    }
    /// Grants are always returned for execution outside the manager's allocator mutex.
    /// `concurrency.ts:118 park`: gives the slot back and dispatches eligible lanes. Grants are
    /// returned rather than run so callers can execute them outside the allocator mutex.
    pub fn park(&mut self, task_id: &str, epoch: i64) -> (Option<ParkedLease>, Vec<Grant>) {
        let Some(lease) = self.leases.get(&lease_key(task_id, epoch)).cloned() else {
            return (None, Vec::new());
        };
        self.drop_lease(&lease);
        let lease = Arc::new(lease);
        self.parked.insert(
            lease_key(task_id, epoch),
            ParkedEntry {
                lease: Arc::clone(&lease),
                resumable: false,
                resumed: Vec::new(),
            },
        );
        (Some(lease), self.drain())
    }
    pub fn unpark(&mut self, lease: &ParkedLease) -> Receiver<()> {
        let (tx, rx) = mpsc::channel();
        if let Some(entry) = self
            .parked
            .get_mut(&lease_key(&lease.task_id, lease.run_epoch))
            && Arc::ptr_eq(&entry.lease, lease)
        {
            entry.resumable = true;
            entry.resumed.push(tx);
            return rx;
        }
        let _ = tx.send(());
        rx
    }
    pub fn suspend_unpark(&mut self, lease: &ParkedLease) {
        if let Some(entry) = self
            .parked
            .get_mut(&lease_key(&lease.task_id, lease.run_epoch))
            && Arc::ptr_eq(&entry.lease, lease)
            && entry.resumable
        {
            entry.resumable = false;
            entry.settle();
        }
    }
    pub fn release_lease(&mut self, task_id: &str, epoch: i64) -> Vec<Grant> {
        let key = lease_key(task_id, epoch);
        if let Some(lease) = self.leases.get(&key).cloned() {
            self.drop_lease(&lease);
        }
        if let Some(mut entry) = self.parked.remove(&key) {
            entry.settle();
        }
        self.drain()
    }
    pub fn transfer_lease(&mut self, task_id: &str, from: i64, to: i64) -> bool {
        if self.lease_state(task_id, to).is_some() {
            return false;
        }
        let Some(mut lease) = self.leases.remove(&lease_key(task_id, from)) else {
            return false;
        };
        lease.run_epoch = to;
        self.leases.insert(lease_key(task_id, to), lease);
        true
    }
    pub fn release_task(&mut self, task_id: &str) -> Vec<Grant> {
        let leases: Vec<_> = self
            .leases
            .values()
            .filter(|l| l.task_id == task_id)
            .cloned()
            .collect();
        for lease in leases {
            self.drop_lease(&lease);
        }
        let keys: Vec<_> = self
            .parked
            .keys()
            .filter(|(id, _)| id == task_id)
            .cloned()
            .collect();
        for key in keys {
            if let Some(mut entry) = self.parked.remove(&key) {
                entry.settle();
            }
        }
        self.drain()
    }
    pub fn release(&mut self, model: &str) -> Option<Grant> {
        let lane = self.get_key(model);
        let key = self
            .leases
            .iter()
            .filter(|(_, l)| l.lane_key == lane)
            .min_by_key(|(_, l)| l.sequence)
            .map(|(k, _)| k.clone())
            .or_else(|| {
                self.parked
                    .iter()
                    .find(|(_, e)| e.lease.lane_key == lane)
                    .map(|(k, _)| k.clone())
            });
        let grants = key.map_or_else(Vec::new, |(id, epoch)| self.release_lease(&id, epoch));
        if grants.is_empty() {
            None
        } else {
            Some(Box::new(move || {
                for grant in grants {
                    grant();
                }
            }))
        }
    }
    pub fn drain(&mut self) -> Vec<Grant> {
        let mut grants = Vec::new();
        loop {
            // `concurrency.ts:204 #drainEligible`: no global permit, no admission.
            if !self.global_has_room() {
                break;
            }
            let resume = self
                .parked
                .iter()
                .filter(|(_, e)| {
                    e.resumable && self.lane_has_room(&e.lease.model, &e.lease.lane_key)
                })
                .min_by_key(|(_, e)| e.lease.sequence)
                .map(|(k, _)| k.clone());
            if let Some(key) = resume {
                if let Some(mut entry) = self.parked.remove(&key) {
                    self.record_lease((*entry.lease).clone());
                    entry.settle();
                }
                continue;
            }
            let selected = self
                .queues
                .iter()
                .filter_map(|(lane, q)| {
                    q.front()
                        .filter(|w| self.lane_has_room(&w.admission.model, lane))
                        .map(|w| (lane.clone(), w.admission.sequence))
                })
                .min_by_key(|(_, s)| *s)
                .map(|(lane, _)| lane);
            let Some(lane) = selected else {
                break;
            };
            let Some(queue) = self.queues.get_mut(&lane) else {
                break;
            };
            let next = queue.pop_front();
            let empty = queue.is_empty();
            if empty {
                self.queues.remove(&lane);
            }
            if let Some(waiter) = next {
                self.record_lease(waiter.admission);
                grants.push(waiter.grant);
            }
        }
        grants
    }
    pub fn get_count(&self, model: &str) -> usize {
        self.counts.get(self.key_of(model)).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod custody_tests {
    use super::*;
    #[test]
    fn cold_resume_requeues_saved_lane_sequence_and_withdraws_exact_epoch() {
        let mut concurrency = TaskConcurrency::new(TaskConcurrencyConfig {
            default_concurrency: Some(1),
            ..Default::default()
        });
        assert!(concurrency.try_acquire("p/model", "busy", 1));
        let saved = QueuedTaskAdmission {
            model: "p/model".into(),
            lane_key: "p/model".into(),
            task_id: "resumed".into(),
            run_epoch: 9,
            sequence: 0,
        };
        let (send, receive) = mpsc::channel();
        concurrency.requeue(
            saved.clone(),
            Box::new(move || {
                send.send(()).unwrap();
            }),
        );
        assert!(concurrency.withdraw("p/model", "resumed", 8).is_none());
        assert_eq!(
            concurrency.withdraw("p/model", "resumed", 9),
            Some(saved.clone())
        );
        assert!(receive.recv().is_err());
        let (send, receive) = mpsc::channel();
        concurrency.requeue(
            saved.clone(),
            Box::new(move || {
                send.send(()).unwrap();
            }),
        );
        for grant in concurrency.release_lease("busy", 1) {
            grant();
        }
        receive
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            concurrency.admission("resumed", 9),
            Some((saved, AdmissionPhase::PostGrant))
        );
    }

    #[test]
    fn withdrawn_admissions_restore_original_fifo_and_epoch() {
        let mut c = TaskConcurrency::new(TaskConcurrencyConfig {
            default_concurrency: Some(1),
            ..Default::default()
        });
        assert!(c.try_acquire("p/m", "owner", 1));
        c.enqueue_epoch("p/m", "first", 2, Box::new(|| {}));
        c.enqueue_epoch("p/m", "second", 3, Box::new(|| {}));
        assert!(c.withdraw("p/m", "first", 99).is_none());
        let first = c.withdraw("p/m", "first", 2).expect("first");
        let second = c.withdraw("p/m", "second", 3).expect("second");
        c.requeue(second, Box::new(|| {}));
        c.requeue(first.clone(), Box::new(|| {}));
        assert_eq!(c.queue_position("p/m", "first"), Some(1));
        let grants = c.release_lease("owner", 1);
        assert_eq!(grants.len(), 1);
        assert_eq!(
            c.admission("first", 2),
            Some((first, AdmissionPhase::PostGrant))
        );
    }
    #[test]
    fn second_stop_settles_resume_but_keeps_parked_identity() {
        let mut c = TaskConcurrency::new(TaskConcurrencyConfig {
            default_concurrency: Some(1),
            ..Default::default()
        });
        assert!(c.try_acquire("p/m", "child", 1));
        let saved = c.admission("child", 1).expect("lease").0;
        let (lease, grants) = c.park("child", 1);
        assert!(grants.is_empty());
        let lease = lease.expect("park");
        assert!(c.try_acquire("p/m", "other", 2));
        let resumed = c.unpark(&lease);
        c.suspend_unpark(&lease);
        resumed
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("suspended waiter");
        c.release_lease("other", 2);
        assert_eq!(c.lease_state("child", 1), Some("parked"));
        let resumed = c.unpark(&lease);
        c.drain();
        resumed
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("resumed");
        assert_eq!(
            c.admission("child", 1),
            Some((saved, AdmissionPhase::PostGrant))
        );
        c.release_lease("child", 1);
        let stale = c.unpark(&lease);
        stale
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("stale token settles");
        assert_eq!(c.get_count("p/m"), 0);
    }

    #[test]
    fn global_cap_limits_total_leases() {
        let mut c = TaskConcurrency::new(TaskConcurrencyConfig {
            default_concurrency: Some(5),
            global_concurrency: Some(2),
            ..Default::default()
        });
        assert!(c.try_acquire("a/one", "st_1", 1));
        assert!(c.try_acquire("b/two", "st_2", 1));
        // Both lanes still have room, but the global permit budget is spent (N+1 refused).
        assert!(!c.has_free_slot("a/one"));
        assert!(!c.has_free_slot("c/three"));
        assert!(!c.try_acquire("c/three", "st_3", 1));
        assert_eq!(c.lease_state("st_3", 1), None);
        // Releasing one lease returns exactly one permit to the global budget.
        c.release_lease("st_1", 1);
        assert!(c.has_free_slot("c/three"));
        assert!(c.try_acquire("c/three", "st_3", 1));
        assert_eq!(c.lease_state("st_3", 1), Some("held"));
        assert!(!c.try_acquire("d/four", "st_4", 1));
    }

    #[test]
    fn global_cap_zero_is_unlimited() {
        let mut c = TaskConcurrency::new(TaskConcurrencyConfig {
            default_concurrency: Some(1),
            global_concurrency: Some(0),
            ..Default::default()
        });
        for index in 0..20 {
            let model = format!("p{index}/model");
            assert!(c.has_free_slot(&model), "{model} should have a permit");
            assert!(c.try_acquire(&model, &format!("st_{index}"), 1));
        }
    }

    #[test]
    fn opengateway_is_unbounded_by_default() {
        assert_eq!(
            TaskConcurrency::new(TaskConcurrencyConfig {
                default_concurrency: Some(1),
                ..Default::default()
            })
            .get_limit("opengateway/glm-5.2-ultrafast"),
            None
        );
        let mut c = TaskConcurrency::new(TaskConcurrencyConfig {
            default_concurrency: Some(1),
            ..Default::default()
        });
        for index in 0..5 {
            let model = format!("opengateway/model-{index}");
            assert!(c.has_free_slot(&model));
            assert!(c.try_acquire(&model, &format!("og_{index}"), 1));
        }
        // No per-lane count either: a second lease on the same unlimited lane is admitted.
        assert!(c.try_acquire("opengateway/model-0", "og_same_lane", 1));
        // An explicit provider override still wins over the implicit opengateway default.
        let capped = TaskConcurrency::new(TaskConcurrencyConfig {
            default_concurrency: Some(1),
            provider_concurrency: Some(BTreeMap::from([("opengateway".to_string(), 2)])),
            ..Default::default()
        });
        assert_eq!(capped.get_limit("opengateway/model-0"), Some(2));
    }

    #[test]
    fn park_drains_eligible_lanes() {
        let mut c = TaskConcurrency::new(TaskConcurrencyConfig {
            default_concurrency: Some(5),
            global_concurrency: Some(1),
            ..Default::default()
        });
        assert!(c.try_acquire("a/one", "holder", 1));
        let (send, receive) = mpsc::channel();
        c.enqueue_epoch(
            "b/two",
            "waiter",
            2,
            Box::new(move || {
                send.send(()).unwrap();
            }),
        );
        // The waiter is queued: the single global permit is held by the other lane.
        assert_eq!(
            c.admission("waiter", 2).map(|(_, phase)| phase),
            Some(AdmissionPhase::PreGrant)
        );
        let (lease, grants) = c.park("holder", 1);
        assert!(lease.is_some());
        assert_eq!(grants.len(), 1);
        // The permit is transferred, not freed: drain promotes the waiter and records its lease
        // before returning the grant, so the global slot is already accounted for the promoted
        // lane. Nothing else may admit into that slot (asserted below via the third lane).
        assert_eq!(
            c.active_global, 1,
            "park must hand the global permit to the drained lane"
        );
        for grant in grants {
            grant();
        }
        receive
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("park drains the eligible lane");
        assert_eq!(
            c.admission("waiter", 2).map(|(_, phase)| phase),
            Some(AdmissionPhase::PostGrant)
        );
        assert_eq!(c.lease_state("holder", 1), Some("parked"));
        // The permit was transferred, not duplicated: a third lane stays blocked.
        assert!(!c.has_free_slot("c/three"));
        assert!(!c.try_acquire("c/three", "other", 1));
        c.release_lease("waiter", 2);
        assert!(c.has_free_slot("c/three"));
    }
}
