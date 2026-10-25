//! A session's durable work registry. Tokens never acknowledge their own delivery.
use crate::work_custody_record::{
    CustodyPayload, CustodyState as State, ObligationKind, WorkCustodyRecord, custody_is_open,
    read_work_custody,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketAdmission {
    Admitted,
    Unknown,
    Closed,
    Consumed,
    Delivering,
}
#[derive(Debug, Clone, thiserror::Error)]
#[error("Delivery ticket {ticket_id} was refused: {reason:?}")]
pub struct DeliveryTicketRefused {
    pub ticket_id: String,
    pub reason: TicketAdmission,
}
#[derive(Debug, Clone, thiserror::Error)]
#[error("Work custody is no longer owned: {key}")]
pub struct CustodyViolation {
    pub key: String,
}
pub trait CustodyStore: Send {
    fn entries(&self) -> Vec<Value>;
    fn append(&mut self, record: &WorkCustodyRecord) -> Result<(), String>;
    fn changed(&mut self);
}
#[derive(Debug, Clone)]
pub struct ObligationToken {
    pub id: String,
}
pub struct SessionWorkObligations {
    records: BTreeMap<String, WorkCustodyRecord>,
    unpersisted: BTreeMap<String, WorkCustodyRecord>,
    delivering: BTreeSet<String>,
    published_sources: BTreeSet<String>,
    store: Box<dyn CustodyStore>,
}
impl SessionWorkObligations {
    pub fn new(store: Box<dyn CustodyStore>) -> Self {
        let mut records = BTreeMap::new();
        for entry in store.entries() {
            if entry["type"] == "custom" && entry["customType"] == "work-custody"
                && let Some(record) = read_work_custody(&entry["data"]) {
                    records.insert(record.id.clone(), record);
                }
        }
        Self {
            records,
            unpersisted: BTreeMap::new(),
            delivering: BTreeSet::new(),
            published_sources: BTreeSet::new(),
            store,
        }
    }
    pub fn active(&self) -> Vec<&WorkCustodyRecord> {
        self.records
            .values()
            .filter(|r| custody_is_open(r))
            .collect()
    }
    pub fn active_count(&self) -> f64 {
        self.active()
            .iter()
            .map(|r| r.active_count.unwrap_or(1.))
            .sum()
    }
    pub fn persisted(&self) -> bool {
        self.unpersisted.is_empty()
    }
    pub fn open(
        &mut self,
        key: &str,
        kind: ObligationKind,
        detail: Option<String>,
    ) -> ObligationToken {
        if let Some(r) = self.active().into_iter().find(|r| r.key == key) {
            return ObligationToken { id: r.id.clone() };
        }
        let mut record = fresh(key, kind);
        record.detail = detail;
        let id = record.id.clone();
        self.install(record);
        self.store.changed();
        ObligationToken { id }
    }
    pub fn sync_source(&mut self, source: &str, kind: ObligationKind, active_count: f64) {
        let key = format!("wake-source:{source}");
        self.published_sources.insert(key.clone());
        let current = self.active().into_iter().find(|r| r.key == key).cloned();
        if active_count <= 0. {
            if let Some(r) = current {
                self.close(&r.id, State::Passive, None)
                    .expect("active record");
            }
            return;
        }
        if current
            .as_ref()
            .is_some_and(|r| r.active_count == Some(active_count) && r.kind == kind)
        {
            return;
        }
        let mut record = current.unwrap_or_else(|| fresh(&key, kind));
        record.kind = kind;
        record.source = Some(source.into());
        record.active_count = Some(active_count);
        self.install(record);
        self.store.changed();
    }
    pub fn retire_restored_sources(&mut self) {
        let ids: Vec<_> = self
            .active()
            .into_iter()
            .filter(|r| {
                r.key.starts_with("wake-source:") && !self.published_sources.contains(&r.key)
            })
            .map(|r| r.id.clone())
            .collect();
        for id in ids {
            self.close(&id, State::Passive, None)
                .expect("active record");
        }
    }
    pub fn admission(&self, ticket: &str) -> TicketAdmission {
        let Some(r) = self
            .records
            .values()
            .find(|r| r.ticket_id.as_deref() == Some(ticket))
        else {
            return TicketAdmission::Unknown;
        };
        if !custody_is_open(r) {
            TicketAdmission::Closed
        } else if r.state == State::Consumed {
            TicketAdmission::Consumed
        } else if self.delivering.contains(ticket) {
            TicketAdmission::Delivering
        } else {
            TicketAdmission::Admitted
        }
    }
    pub fn queue(
        &mut self,
        ticket: &str,
        payload: CustodyPayload,
    ) -> Result<bool, CustodyViolation> {
        match self.admission(ticket) {
            TicketAdmission::Unknown => return Err(violation(ticket)),
            TicketAdmission::Admitted => {}
            _ => return Ok(false),
        }
        let mut r = self.ticket_record(ticket)?;
        r.state = State::Queued;
        r.payload = Some(payload);
        r.delivery = None;
        self.install(r);
        self.delivering.insert(ticket.into());
        self.store.changed();
        Ok(true)
    }
    pub fn end_delivery(&mut self, ticket: &str) {
        self.delivering.remove(ticket);
    }
    pub fn passive(&mut self, ticket: &str) -> Result<(), CustodyViolation> {
        let r = self.ticket_record(ticket)?;
        if custody_is_open(&r) {
            self.close(&r.id, State::Passive, None)?;
        }
        Ok(())
    }
    pub fn consumed(
        &mut self,
        ticket: &str,
        run_id: &str,
        transcript_entry_id: &str,
    ) -> Result<(), CustodyViolation> {
        let mut r = self.ticket_record(ticket)?;
        if !custody_is_open(&r) || r.state == State::Consumed {
            return Ok(());
        }
        r.state = State::Consumed;
        r.run_id = Some(run_id.into());
        r.transcript_entry_id = Some(transcript_entry_id.into());
        r.delivery = None;
        self.install(r);
        self.store.changed();
        Ok(())
    }
    pub fn failed(&mut self, ticket: &str) -> Result<(), CustodyViolation> {
        let mut r = self.ticket_record(ticket)?;
        if !custody_is_open(&r) || r.state == State::Consumed {
            return Ok(());
        }
        r.state = State::Queued;
        r.delivery = Some("failed".into());
        self.install(r);
        self.store.changed();
        Ok(())
    }
    pub fn held(&mut self, ticket: &str, notice: &str) -> Result<(), CustodyViolation> {
        let r = self.ticket_record(ticket)?;
        if custody_is_open(&r) {
            self.close(&r.id, State::Held, Some(notice.into()))?;
        }
        Ok(())
    }
    pub fn retire(&mut self, ticket: &str) -> Result<(), CustodyViolation> {
        let mut r = self.ticket_record(ticket)?;
        if !custody_is_open(&r) || r.state == State::Consumed {
            return Ok(());
        }
        r.state = State::Open;
        r.delivery = Some("retired".into());
        r.payload = None;
        self.install(r);
        self.store.changed();
        Ok(())
    }
    pub fn cancel(&mut self, ticket: &str) -> Result<(), CustodyViolation> {
        let r = self.ticket_record(ticket)?;
        if custody_is_open(&r) && r.state != State::Consumed {
            self.close(&r.id, State::Cancelled, None)?;
        }
        Ok(())
    }
    pub fn settle_run(&mut self, run: &str) {
        let records: Vec<_> = self
            .active()
            .into_iter()
            .filter(|r| r.state == State::Consumed && r.run_id.as_deref() == Some(run))
            .cloned()
            .collect();
        for mut r in records {
            r.state = State::Settled;
            self.install(r);
        }
        self.store.changed();
    }
    fn install(&mut self, record: WorkCustodyRecord) {
        if self.store.append(&record).is_ok() {
            self.unpersisted.remove(&record.id);
        } else {
            self.unpersisted.insert(record.id.clone(), record.clone());
        }
        self.records.insert(record.id.clone(), record);
    }
    pub fn flush(&mut self) -> Result<(), String> {
        let pending: Vec<_> = self.unpersisted.values().cloned().collect();
        for r in pending {
            self.store.append(&r)?;
            self.unpersisted.remove(&r.id);
        }
        Ok(())
    }
    fn owned(&self, id: &str) -> Result<WorkCustodyRecord, CustodyViolation> {
        let r = self.records.get(id);
        r.filter(|r| custody_is_open(r))
            .cloned()
            .ok_or_else(|| violation(r.map_or(id, |r| &r.key)))
    }
    fn ticket_record(&self, ticket: &str) -> Result<WorkCustodyRecord, CustodyViolation> {
        self.records
            .values()
            .find(|r| r.ticket_id.as_deref() == Some(ticket))
            .cloned()
            .ok_or_else(|| violation(ticket))
    }
    fn close(
        &mut self,
        id: &str,
        state: State,
        notice_id: Option<String>,
    ) -> Result<(), CustodyViolation> {
        let mut r = self.owned(id)?;
        r.state = state;
        if notice_id.is_some() {
            r.notice_id = notice_id;
        }
        self.install(r);
        self.store.changed();
        Ok(())
    }
}
impl ObligationToken {
    pub fn transfer(
        &self,
        registry: &mut SessionWorkObligations,
        key: &str,
        kind: Option<ObligationKind>,
    ) -> Result<Self, CustodyViolation> {
        let mut current = registry.owned(&self.id)?;
        if current.key == key {
            return Err(violation(key));
        }
        let next = registry
            .active()
            .into_iter()
            .find(|r| r.key == key)
            .cloned()
            .unwrap_or_else(|| fresh(key, kind.unwrap_or(current.kind)));
        let id = next.id.clone();
        registry.install(next);
        current.state = State::Cancelled;
        registry.install(current);
        registry.store.changed();
        Ok(Self { id })
    }
    pub fn ticket(
        &self,
        registry: &mut SessionWorkObligations,
    ) -> Result<String, CustodyViolation> {
        let mut r = registry.owned(&self.id)?;
        if let Some(ticket) = r.ticket_id {
            return Ok(ticket);
        }
        let ticket = uuid::Uuid::new_v4().to_string();
        r.state = State::Queued;
        r.ticket_id = Some(ticket.clone());
        registry.install(r);
        registry.store.changed();
        Ok(ticket)
    }
    pub fn held_as_notice(
        &self,
        registry: &mut SessionWorkObligations,
        notice: &str,
    ) -> Result<(), CustodyViolation> {
        registry.close(&self.id, State::Held, Some(notice.into()))
    }
    pub fn close(
        &self,
        registry: &mut SessionWorkObligations,
        reason: State,
    ) -> Result<(), CustodyViolation> {
        let reason = match reason {
            State::Consumed => State::Settled,
            State::Settled | State::Held | State::Cancelled | State::Passive => reason,
            _ => return Err(violation(&self.id)),
        };
        registry.close(&self.id, reason, None)
    }
}
fn violation(key: &str) -> CustodyViolation {
    CustodyViolation { key: key.into() }
}
fn fresh(key: &str, kind: ObligationKind) -> WorkCustodyRecord {
    WorkCustodyRecord {
        id: uuid::Uuid::new_v4().to_string(),
        key: key.into(),
        kind,
        state: State::Open,
        source: None,
        active_count: None,
        detail: None,
        ticket_id: None,
        payload: None,
        run_id: None,
        transcript_entry_id: None,
        notice_id: None,
        delivery: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Data {
        entries: Vec<Value>,
        fail: bool,
        changed: usize,
    }
    struct Store(Arc<Mutex<Data>>);
    impl CustodyStore for Store {
        fn entries(&self) -> Vec<Value> {
            self.0.lock().unwrap().entries.clone()
        }
        fn append(&mut self, r: &WorkCustodyRecord) -> Result<(), String> {
            let mut data = self.0.lock().unwrap();
            if data.fail {
                return Err("write refused".into());
            }
            data.entries
                .push(serde_json::json!({"type":"custom","customType":"work-custody","data":r}));
            Ok(())
        }
        fn changed(&mut self) {
            self.0.lock().unwrap().changed += 1;
        }
    }
    fn registry() -> (SessionWorkObligations, Arc<Mutex<Data>>) {
        let data = Arc::new(Mutex::new(Data::default()));
        (
            SessionWorkObligations::new(Box::new(Store(data.clone()))),
            data,
        )
    }
    fn payload() -> CustodyPayload {
        CustodyPayload {
            message: serde_json::json!({"customType":"work","content":"done","display":false}),
            deliver_as: None,
        }
    }
    #[test]
    fn admission_and_delivery_require_explicit_acknowledgment() {
        let (mut r, _) = registry();
        assert_eq!(r.admission("missing"), TicketAdmission::Unknown);
        assert!(r.queue("missing", payload()).is_err());
        let token = r.open("child", ObligationKind::Children, None);
        let ticket = token.ticket(&mut r).unwrap();
        assert_eq!(token.ticket(&mut r).unwrap(), ticket);
        assert!(r.queue(&ticket, payload()).unwrap());
        assert_eq!(r.admission(&ticket), TicketAdmission::Delivering);
        assert!(!r.queue(&ticket, payload()).unwrap());
        r.end_delivery(&ticket);
        r.failed(&ticket).unwrap();
        assert!(r.queue(&ticket, payload()).unwrap());
        r.consumed(&ticket, "run", "entry").unwrap();
        r.cancel(&ticket).unwrap();
        r.retire(&ticket).unwrap();
        assert_eq!(r.admission(&ticket), TicketAdmission::Consumed);
        assert_eq!(r.active_count(), 1.);
        r.settle_run("other");
        assert_eq!(r.active_count(), 1.);
        r.settle_run("run");
        assert_eq!(r.admission(&ticket), TicketAdmission::Closed);
        assert_eq!(r.active_count(), 0.);
    }
    #[test]
    fn persistence_failure_preserves_live_work_and_flush_retries() {
        let (mut r, data) = registry();
        data.lock().unwrap().fail = true;
        let token = r.open("work", ObligationKind::Background, None);
        assert!(!r.persisted());
        assert_eq!(r.active_count(), 1.);
        assert!(r.flush().is_err());
        data.lock().unwrap().fail = false;
        r.flush().unwrap();
        assert!(r.persisted());
        let next = token.transfer(&mut r, "next", None).unwrap();
        assert!(token.ticket(&mut r).is_err());
        assert!(next.transfer(&mut r, "next", None).is_err());
        let restored = SessionWorkObligations::new(Box::new(Store(data)));
        assert_eq!(restored.active_count(), 1.);
    }
    #[test]
    fn restored_sources_retire_only_if_not_republished() {
        let (mut r, data) = registry();
        r.sync_source("a", ObligationKind::Children, 2.);
        r.sync_source("b", ObligationKind::Background, 3.);
        let mut restored = SessionWorkObligations::new(Box::new(Store(data)));
        restored.sync_source("a", ObligationKind::Children, 2.);
        restored.retire_restored_sources();
        assert_eq!(restored.active_count(), 2.);
        restored.sync_source("a", ObligationKind::Children, 0.);
        assert_eq!(restored.active_count(), 0.);
    }
    #[test]
    fn retirement_and_notices_close_the_expected_states() {
        let (mut r, _) = registry();
        let token = r.open("work", ObligationKind::Background, None);
        let ticket = token.ticket(&mut r).unwrap();
        r.queue(&ticket, payload()).unwrap();
        r.end_delivery(&ticket);
        r.retire(&ticket).unwrap();
        assert_eq!(r.admission(&ticket), TicketAdmission::Admitted);
        assert!(r.active()[0].payload.is_none());
        r.held(&ticket, "notice").unwrap();
        assert_eq!(r.admission(&ticket), TicketAdmission::Closed);
    }
}
