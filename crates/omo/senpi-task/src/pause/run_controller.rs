//! Whole-run pause transaction coordinator (`pause/run-controller.ts`).
use super::{
    admission_barrier::{AdmissionParticipant, AdmissionState, ResumeCredential},
    ports::{ObservedHold, RunPauseOwner, RunRef},
    registry::*,
};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
};
pub type Effect<'a, T> = Pin<Box<dyn Future<Output = Result<T, PauseError>> + Send + 'a>>;
pub struct ResumeParticipantRequest {
    pub owner: RunPauseOwner,
    pub resume_id: String,
    pub child_seq_cutoff: Option<u64>,
}
pub enum PauseParticipantResult {
    Held { child_seq: u64 },
    Settled,
    Pending,
}
pub enum ResumeParticipantResult {
    Resumed { turn_id: Option<String> },
    HeldByUser { child_seq: u64 },
    Settled,
}
pub trait RunPauseEffects: Send + Sync {
    fn snapshot(&self, parent: &str, admissions: &[AdmissionParticipant]) -> Vec<PauseParticipant>;
    fn quiesce<'a>(&'a self, parent: &'a str, seq: u64) -> Effect<'a, ()>;
    fn settle_creations<'a>(&'a self, _parent: &'a str, _seq: u64) -> Effect<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn add_fences<'a>(&'a self, parent: &'a str, seq: u64, runs: Vec<RunRef>) -> Effect<'a, ()>;
    fn pause<'a>(
        &'a self,
        participant: &'a PauseParticipant,
        owner: RunPauseOwner,
    ) -> Effect<'a, PauseParticipantResult>;
    fn dispatch_guidance<'a>(
        &'a self,
        parent: &'a str,
        seq: u64,
        resume_id: &'a str,
    ) -> Effect<'a, ()>;
    fn resume<'a>(
        &'a self,
        participant: &'a PauseParticipant,
        request: ResumeParticipantRequest,
        credential: Option<ResumeCredential>,
    ) -> Effect<'a, ResumeParticipantResult>;
    fn install<'a>(
        &'a self,
        parent: &'a str,
        seq: u64,
        credential: ResumeCredential,
    ) -> Effect<'a, ()>;
    fn acknowledge_installed<'a>(
        &'a self,
        parent: &'a str,
        seq: u64,
        resume_id: &'a str,
    ) -> Effect<'a, ()>;
    fn cancel<'a>(&'a self, participant: &'a PauseParticipant, parent: &'a str) -> Effect<'a, ()>;
    fn observe_hold<'a>(
        &'a self,
        _participant: &'a PauseParticipant,
        _parent: &'a str,
    ) -> Effect<'a, Option<ObservedHold>> {
        Box::pin(async { Ok(None) })
    }
}
struct ResumeTransaction {
    pause_seq: u64,
    resume_id: String,
    credential: ResumeCredential,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeFailure {
    pub key: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResumeResult {
    Installed { failures: Vec<ResumeFailure> },
    Superseded,
}
enum ResumeOne {
    Done,
    Failure(ResumeFailure),
    Superseded,
}
pub struct RunPauseController {
    pub registry: PauseRegistry,
    pub effects: Arc<dyn RunPauseEffects>,
    resuming: Mutex<BTreeMap<String, Arc<ResumeTransaction>>>,
    released: Mutex<BTreeMap<String, Arc<ResumeTransaction>>>,
}
impl RunPauseController {
    pub fn new(registry: PauseRegistry, effects: Arc<dyn RunPauseEffects>) -> Self {
        Self {
            registry,
            effects,
            resuming: Mutex::new(BTreeMap::new()),
            released: Mutex::new(BTreeMap::new()),
        }
    }
    pub fn pause<'a>(&'a self, parent: &'a str, seq: u64) -> Effect<'a, ()> {
        let admissions = self.registry.barrier.close(parent, seq);
        Box::pin(async move {
            let prior = self.registry.load(parent)?;
            if prior.as_ref().is_some_and(|p| {
                p.pause_seq > seq || (p.pause_seq == seq && p.phase != PausePhase::Propagating)
            }) {
                return Ok(());
            }
            self.resuming
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(parent);
            let record = match prior {
                Some(p) if p.pause_seq == seq => p,
                _ => {
                    self.registry
                        .begin(parent, seq, self.effects.snapshot(parent, &admissions))?
                }
            };
            // Poll both effects concurrently: quiescence must start before fence installation finishes.
            let quiesced = self.effects.quiesce(parent, seq);
            let mut runs = Vec::new();
            for p in record.participants.values() {
                if !runs.contains(&p.run_ref) {
                    runs.push(p.run_ref.clone());
                }
            }
            let fences = self.effects.add_fences(parent, seq, runs);
            let (quiesce_result, fence_result) = tokio::join!(quiesced, fences);
            fence_result?;
            quiesce_result?;
            if self.registry.require(parent)?.pause_seq != seq
                || !self.propagate(parent, seq).await?
            {
                return Ok(());
            }
            self.effects.settle_creations(parent, seq).await?;
            if self.registry.require(parent)?.pause_seq != seq
                || !self.propagate(parent, seq).await?
            {
                return Ok(());
            }
            for participant in self.registry.require(parent)?.participants.into_values() {
                if matches!(
                    participant.state,
                    ParticipantState::Held | ParticipantState::Restorable | ParticipantState::Lost
                ) {
                    continue;
                }
                self.hold_or_lose(parent, seq, &participant, "never acknowledged the pause")
                    .await?;
            }
            self.registry.phase(parent, seq, PausePhase::Applied)?;
            Ok(())
        })
    }
    pub fn record_propagation_failure(
        &self,
        parent: &str,
        seq: u64,
        error: &dyn std::fmt::Display,
    ) -> Result<(), PauseError> {
        let reason = format!("the Stop's propagation failed: {error}");
        eprintln!("[senpi-task] run pause propagation failed: {parent} {seq}: {reason}");
        let Some(record) = self.registry.load(parent)? else {
            return Ok(());
        };
        if record.pause_seq != seq {
            return Ok(());
        }
        for p in record
            .participants
            .values()
            .filter(|p| p.state == ParticipantState::Pending)
        {
            self.registry.update(parent, seq, &p.key, |p| {
                p.state = ParticipantState::Lost;
                p.lost_reason = Some(reason.clone());
            })?;
        }
        Ok(())
    }
    pub async fn resume(
        &self,
        parent: &str,
        seq: u64,
        resume_id: &str,
    ) -> Result<RunResumeResult, PauseError> {
        let requested = self.registry.request_resume(parent, seq, resume_id)?;
        self.effects
            .dispatch_guidance(parent, seq, resume_id)
            .await?;
        let after = self.registry.require(parent)?;
        if after.pause_seq != seq
            || after
                .resume
                .as_ref()
                .is_none_or(|r| r.resume_id != resume_id)
        {
            return Ok(RunResumeResult::Superseded);
        }
        let credential = self.registry.authorize_resume(parent, seq, resume_id)?;
        let transaction = Arc::new(ResumeTransaction {
            pause_seq: seq,
            resume_id: resume_id.into(),
            credential: credential.clone(),
        });
        self.resuming
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(parent.into(), transaction.clone());
        let mut failures = vec![];
        for participant in self.registry.ordered_admissions(parent)? {
            if !self.current(parent, &transaction)? {
                return Ok(RunResumeResult::Superseded);
            }
            if participant.state == ParticipantState::Resumed {
                continue;
            }
            match self
                .resume_one(
                    parent,
                    seq,
                    resume_id,
                    &participant,
                    requested.child_seq_cutoffs.get(&participant.key).copied(),
                    Some(credential.clone()),
                    || self.current(parent, &transaction),
                )
                .await?
            {
                ResumeOne::Superseded => return Ok(RunResumeResult::Superseded),
                ResumeOne::Failure(failure) => failures.push(failure),
                ResumeOne::Done => {}
            }
        }
        self.effects.install(parent, seq, credential).await?;
        if !self.current(parent, &transaction)? {
            return Ok(RunResumeResult::Superseded);
        }
        self.effects
            .acknowledge_installed(parent, seq, resume_id)
            .await?;
        let record = self.registry.require(parent)?;
        if record.pause_seq != seq
            || record
                .resume
                .as_ref()
                .is_none_or(|r| r.resume_id != resume_id)
        {
            return Ok(RunResumeResult::Superseded);
        }
        Ok(RunResumeResult::Installed { failures })
    }
    pub async fn retry_lost(
        &self,
        parent: &str,
        seq: u64,
    ) -> Result<Vec<ResumeFailure>, PauseError> {
        let Some(record) = self.registry.load(parent)? else {
            return Ok(vec![]);
        };
        if record.pause_seq != seq
            || record.phase != PausePhase::Resumed
            || self.registry.barrier.state(parent) != AdmissionState::Open
        {
            return Ok(vec![]);
        }
        let credential = self
            .released
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(parent)
            .filter(|t| t.pause_seq == seq)
            .map(|t| t.credential.clone());
        let resume_id = record
            .resume
            .map_or_else(|| format!("parent-resume:{seq}"), |r| r.resume_id);
        let current = || -> Result<bool, PauseError> {
            Ok(self
                .registry
                .load(parent)?
                .is_some_and(|r| r.pause_seq == seq && r.phase == PausePhase::Resumed)
                && self.registry.barrier.state(parent) == AdmissionState::Open)
        };
        let mut failures = vec![];
        for participant in self.registry.ordered_admissions(parent)? {
            if participant.state != ParticipantState::Lost {
                continue;
            }
            match self
                .resume_one(
                    parent,
                    seq,
                    &resume_id,
                    &participant,
                    None,
                    credential.clone(),
                    current,
                )
                .await?
            {
                ResumeOne::Superseded => break,
                ResumeOne::Failure(failure) => failures.push(failure),
                ResumeOne::Done => {}
            }
        }
        Ok(failures)
    }
    #[allow(clippy::too_many_arguments)]
    async fn resume_one(
        &self,
        parent: &str,
        seq: u64,
        resume_id: &str,
        participant: &PauseParticipant,
        mut cutoff: Option<u64>,
        credential: Option<ResumeCredential>,
        current: impl Fn() -> Result<bool, PauseError>,
    ) -> Result<ResumeOne, PauseError> {
        let outside = matches!(
            participant.state,
            ParticipantState::Lost | ParticipantState::Pending
        );
        if outside && participant.kind != ParticipantKind::HeldAdmission {
            let observed = self.observe(parent, participant).await;
            if !current()? {
                return Ok(ResumeOne::Superseded);
            }
            if !observed.as_ref().is_some_and(|o| o.held) {
                let reason = participant
                    .lost_reason
                    .clone()
                    .unwrap_or_else(|| "never acknowledged the pause".into());
                eprintln!(
                    "[senpi-task] run resume skips a lost participant: {parent} {}: {reason}",
                    participant.key
                );
                if participant.state == ParticipantState::Pending {
                    self.registry.update(parent, seq, &participant.key, |p| {
                        p.state = ParticipantState::Lost;
                        p.lost_reason = Some(reason);
                    })?;
                }
                return Ok(ResumeOne::Done);
            }
            cutoff = observed.map(|o| o.child_seq);
        }
        if participant.kind != ParticipantKind::HeldAdmission && cutoff.is_none() {
            return Err(PauseError::message(format!(
                "missing pause acknowledgement for {}",
                participant.key
            )));
        }
        self.registry.update(parent, seq, &participant.key, |p| {
            p.state = ParticipantState::Resuming
        })?;
        let request = ResumeParticipantRequest {
            owner: RunPauseOwner {
                parent_session_id: parent.into(),
                parent_pause_seq: seq,
                run_ref: participant.run_ref.clone(),
            },
            resume_id: resume_id.into(),
            child_seq_cutoff: cutoff,
        };
        let result = match self.effects.resume(participant, request, credential).await {
            Ok(result) => result,
            Err(error) if outside => {
                let reason = format!("resume failed: {error}");
                eprintln!(
                    "[senpi-task] run resume failed for a lost participant: {parent} {}: {reason}",
                    participant.key
                );
                self.registry.update(parent, seq, &participant.key, |p| {
                    p.state = ParticipantState::Lost;
                    p.lost_reason = Some(reason.clone());
                })?;
                return Ok(if current()? {
                    ResumeOne::Failure(ResumeFailure {
                        key: participant.key.clone(),
                        reason,
                    })
                } else {
                    ResumeOne::Superseded
                });
            }
            Err(error) => return Err(error),
        };
        if !current()? {
            return Ok(ResumeOne::Superseded);
        }
        match result {
            ResumeParticipantResult::Settled => {
                self.registry.drop(parent, seq, &participant.key)?;
            }
            ResumeParticipantResult::HeldByUser { child_seq } => {
                self.registry.update(parent, seq, &participant.key, |p| {
                    p.state = ParticipantState::HeldByUser;
                    p.observed_child_seq = Some(child_seq);
                })?;
            }
            ResumeParticipantResult::Resumed { turn_id } => {
                self.registry.update(parent, seq, &participant.key, |p| {
                    p.state = ParticipantState::Resumed;
                    p.lost_reason = None;
                    if turn_id.is_some() {
                        p.resume_turn_id = turn_id;
                    }
                })?;
            }
        }
        Ok(ResumeOne::Done)
    }
    pub fn released(&self, parent: &str, seq: u64, resume_id: &str) -> Result<bool, PauseError> {
        let mut resuming = self.resuming.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(transaction) = resuming.get(parent) else {
            return Ok(false);
        };
        if transaction.pause_seq != seq
            || transaction.resume_id != resume_id
            || !self
                .registry
                .barrier
                .release(parent, &transaction.credential)
        {
            return Ok(false);
        }
        self.registry.phase(parent, seq, PausePhase::Resumed)?;
        if let Some(transaction) = resuming.remove(parent) {
            self.released
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(parent.into(), transaction);
        }
        Ok(true)
    }
    pub async fn cancel(&self, parent: &str, run_ref: &RunRef) -> Result<(), PauseError> {
        for participant in self
            .registry
            .require(parent)?
            .participants
            .into_values()
            .filter(|p| &p.run_ref == run_ref)
        {
            self.effects.cancel(&participant, parent).await?;
            let current = self.registry.require(parent)?;
            self.registry
                .drop(parent, current.pause_seq, &participant.key)?;
        }
        Ok(())
    }
    async fn propagate(&self, parent: &str, seq: u64) -> Result<bool, PauseError> {
        for key in self.registry.require(parent)?.participants.keys() {
            let record = self.registry.require(parent)?;
            if record.pause_seq != seq {
                return Ok(false);
            }
            let Some(participant) = record.participants.get(key) else {
                continue;
            };
            if matches!(
                participant.state,
                ParticipantState::Held | ParticipantState::Restorable | ParticipantState::Lost
            ) {
                continue;
            }
            if participant.kind == ParticipantKind::HeldAdmission {
                self.registry.update(parent, seq, key, |p| {
                    p.state = ParticipantState::Held;
                    p.ack_seq = Some(seq);
                })?;
                continue;
            }
            let result = self
                .effects
                .pause(
                    participant,
                    RunPauseOwner {
                        parent_session_id: parent.into(),
                        parent_pause_seq: seq,
                        run_ref: participant.run_ref.clone(),
                    },
                )
                .await;
            if self.registry.require(parent)?.pause_seq != seq {
                return Ok(false);
            }
            match result {
                Err(error) => {
                    self.hold_or_lose(parent, seq, participant, &error.to_string())
                        .await?
                }
                Ok(PauseParticipantResult::Settled) => {
                    self.registry.drop(parent, seq, key)?;
                }
                Ok(PauseParticipantResult::Held { child_seq }) => {
                    self.registry.update(parent, seq, key, |p| {
                        p.state = ParticipantState::Held;
                        p.ack_seq = Some(seq);
                        p.observed_child_seq = Some(child_seq);
                    })?;
                }
                Ok(PauseParticipantResult::Pending) => {}
            }
        }
        Ok(true)
    }
    async fn observe(&self, parent: &str, participant: &PauseParticipant) -> Option<ObservedHold> {
        match self.effects.observe_hold(participant, parent).await {
            Ok(observed) => observed,
            Err(error) => {
                eprintln!(
                    "[senpi-task] run hold observation failed: {parent} {}: {error}",
                    participant.key
                );
                None
            }
        }
    }
    async fn hold_or_lose(
        &self,
        parent: &str,
        seq: u64,
        participant: &PauseParticipant,
        reason: &str,
    ) -> Result<(), PauseError> {
        let observed = self.observe(parent, participant).await;
        if self.registry.require(parent)?.pause_seq != seq {
            return Ok(());
        }
        if let Some(observed) = observed.filter(|o| o.held) {
            self.registry.update(parent, seq, &participant.key, |p| {
                p.state = ParticipantState::Held;
                p.ack_seq = Some(seq);
                p.observed_child_seq = Some(observed.child_seq);
            })?;
        } else {
            eprintln!(
                "[senpi-task] run pause lost a participant: {parent} {}: {reason}",
                participant.key
            );
            self.registry.update(parent, seq, &participant.key, |p| {
                p.state = ParticipantState::Lost;
                p.lost_reason = Some(reason.into());
            })?;
        }
        Ok(())
    }
    fn current(
        &self,
        parent: &str,
        transaction: &Arc<ResumeTransaction>,
    ) -> Result<bool, PauseError> {
        let same = self
            .resuming
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(parent)
            .is_some_and(|t| Arc::ptr_eq(t, transaction));
        Ok(same
            && self.registry.barrier.state(parent)
                == (AdmissionState::Resuming {
                    pause_seq: transaction.pause_seq,
                    resume_id: transaction.resume_id.clone(),
                })
            && self.registry.require(parent)?.pause_seq == transaction.pause_seq)
    }
}
