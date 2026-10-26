//! The per-root admission barrier: every start enters it before anything is created, so a pause
//! that lands mid-start can hold the start instead of racing it (`pause/admission-barrier.ts`).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

use super::ports::{RunPauseOwner, RunRef};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AdmissionState {
    Open,
    Closed { pause_seq: u64 },
    Resuming { pause_seq: u64, resume_id: String },
}

/// The credential a resumed start presents; `None` means "any".
#[derive(Debug, Clone)]
pub struct ResumeCredential {
    pub pause_seq: u64,
    pub resume_id: String,
    token: Arc<()>,
}

impl PartialEq for ResumeCredential {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
}
impl Eq for ResumeCredential {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionParticipant {
    pub key: String,
    pub run_ref: RunRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionCheck {
    Allowed,
    Held { pause_seq: u64 },
}

/// What a committed start becomes: a launch, or a start that lands paused under the pause owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionCommit {
    Launch,
    StartPaused { owner: RunPauseOwner },
}

#[derive(Clone)]
pub enum AdmissionEntry {
    Entered { ticket: AdmissionTicket },
    Held { pause_seq: u64 },
}

struct RootBarrier {
    state: AdmissionState,
    high_water: u64,
    credential: Option<ResumeCredential>,
    tickets: BTreeMap<u64, AdmissionParticipant>,
    creations: BTreeMap<u64, String>,
    creation_waiters: Vec<oneshot::Sender<()>>,
}

impl RootBarrier {
    fn new() -> Self {
        Self {
            state: AdmissionState::Open,
            high_water: 0,
            credential: None,
            tickets: BTreeMap::new(),
            creations: BTreeMap::new(),
            creation_waiters: Vec::new(),
        }
    }

    /// A start is permitted while the root is open, or while resuming with the matching credential.
    fn permitted(&self, credential: Option<&ResumeCredential>) -> bool {
        let credential_matches = match credential {
            None => true,
            Some(credential) => self.credential.as_ref() == Some(credential),
        };
        if !credential_matches {
            return false;
        }
        match &self.state {
            AdmissionState::Open => true,
            AdmissionState::Resuming {
                pause_seq,
                resume_id,
            } => match credential {
                None => false,
                Some(credential) => {
                    self.credential.as_ref() == Some(credential)
                        && credential.pause_seq == *pause_seq
                        && credential.resume_id == *resume_id
                }
            },
            AdmissionState::Closed { .. } => false,
        }
    }

    fn check(&self, entered_seq: u64, credential: Option<&ResumeCredential>) -> AdmissionCheck {
        if entered_seq == self.high_water && self.permitted(credential) {
            AdmissionCheck::Allowed
        } else {
            AdmissionCheck::Held {
                pause_seq: self.high_water,
            }
        }
    }

    fn participant(&self, key: u64) -> Option<&AdmissionParticipant> {
        self.tickets.get(&key)
    }
}

/// One entered start. Dropping it without [`AdmissionTicket::leave`] leaks the participant, exactly
/// as upstream's `finally { scope.leave() }` exists to prevent.
#[derive(Clone)]
pub struct AdmissionTicket {
    root: Arc<Mutex<RootBarrier>>,
    parent_session_id: String,
    key: u64,
    entered_seq: u64,
    credential: Option<ResumeCredential>,
}

impl AdmissionTicket {
    fn lock(&self) -> MutexGuard<'_, RootBarrier> {
        self.root.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn parent_session_id(&self) -> &str {
        &self.parent_session_id
    }

    /// Declares that this ticket is about to create `task_id` (defaults to the participant key).
    pub fn creating(&self, task_id: Option<&str>) {
        let mut root = self.lock();
        if let Some(participant) = root.participant(self.key) {
            let created = task_id.unwrap_or(&participant.key).to_string();
            root.creations.insert(self.key, created);
        }
    }

    pub fn update(&self, participant: AdmissionParticipant) {
        let mut root = self.lock();
        if root.tickets.contains_key(&self.key) {
            root.tickets.insert(self.key, participant);
        }
    }

    pub fn check(&self) -> AdmissionCheck {
        self.lock()
            .check(self.entered_seq, self.credential.as_ref())
    }

    pub fn commit(&self) -> AdmissionCommit {
        let root = self.lock();
        match root.check(self.entered_seq, self.credential.as_ref()) {
            AdmissionCheck::Allowed => AdmissionCommit::Launch,
            AdmissionCheck::Held { pause_seq } => AdmissionCommit::StartPaused {
                owner: RunPauseOwner {
                    parent_session_id: self.parent_session_id.clone(),
                    parent_pause_seq: pause_seq,
                    run_ref: root
                        .participant(self.key)
                        .map(|participant| participant.run_ref.clone())
                        .unwrap_or_else(|| RunRef::task(String::new())),
                },
            },
        }
    }

    pub fn leave(&self) {
        let mut root = self.lock();
        root.tickets.remove(&self.key);
        if root.creations.remove(&self.key).is_some() && root.creations.is_empty() {
            for waiter in root.creation_waiters.drain(..) {
                let _ = waiter.send(());
            }
        }
    }
}

/// The admission barrier of every parent session, keyed by root session id.
#[derive(Clone, Default)]
pub struct AdmissionBarrier {
    roots: Arc<Mutex<BTreeMap<String, Arc<Mutex<RootBarrier>>>>>,
    next_key: Arc<Mutex<u64>>,
}

impl AdmissionBarrier {
    pub fn new() -> Self {
        Self::default()
    }

    fn root(&self, id: &str) -> Arc<Mutex<RootBarrier>> {
        let mut roots = self.roots.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            roots
                .entry(id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(RootBarrier::new()))),
        )
    }

    fn next_key(&self) -> u64 {
        let mut next = self.next_key.lock().unwrap_or_else(PoisonError::into_inner);
        *next += 1;
        *next
    }

    pub fn state(&self, id: &str) -> AdmissionState {
        self.root(id)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .state
            .clone()
    }

    pub fn in_flight(&self, id: &str) -> Vec<AdmissionParticipant> {
        self.root(id)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tickets
            .values()
            .cloned()
            .collect()
    }

    pub fn enter(
        &self,
        id: &str,
        participant: AdmissionParticipant,
        credential: Option<ResumeCredential>,
    ) -> AdmissionEntry {
        let root = self.root(id);
        let key = self.next_key();
        let mut barrier = root.lock().unwrap_or_else(PoisonError::into_inner);
        let entered_seq = barrier.high_water;
        if !barrier.permitted(credential.as_ref()) {
            return AdmissionEntry::Held {
                pause_seq: barrier.high_water,
            };
        }
        barrier.tickets.insert(key, participant);
        drop(barrier);
        AdmissionEntry::Entered {
            ticket: AdmissionTicket {
                root,
                parent_session_id: id.to_string(),
                key,
                entered_seq,
                credential,
            },
        }
    }

    /// Resolves once no entered start is creating a task under `id`.
    pub async fn creations_settled(&self, id: &str) {
        loop {
            let receiver = {
                let root = self.root(id);
                let mut barrier = root.lock().unwrap_or_else(PoisonError::into_inner);
                if barrier.creations.is_empty() {
                    return;
                }
                let (sender, receiver) = oneshot::channel();
                barrier.creation_waiters.push(sender);
                receiver
            };
            let _ = receiver.await;
        }
    }

    pub fn is_creating(&self, id: &str, task_id: &str) -> bool {
        self.root(id)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .creations
            .values()
            .any(|created| created == task_id)
    }

    /// Closes the root at `pause_seq`; every in-flight participant is reported back to the caller.
    pub fn close(&self, id: &str, pause_seq: u64) -> Vec<AdmissionParticipant> {
        let root = self.root(id);
        let mut barrier = root.lock().unwrap_or_else(PoisonError::into_inner);
        if barrier.high_water >= pause_seq {
            return barrier.tickets.values().cloned().collect();
        }
        barrier.high_water = pause_seq;
        barrier.state = AdmissionState::Closed { pause_seq };
        barrier.credential = None;
        barrier.tickets.values().cloned().collect()
    }

    /// Issues the credential a resumed start must present; a repeated resume id reuses it.
    pub fn resume(
        &self,
        id: &str,
        pause_seq: u64,
        resume_id: &str,
    ) -> Result<ResumeCredential, AdmissionBarrierError> {
        let root = self.root(id);
        let mut barrier = root.lock().unwrap_or_else(PoisonError::into_inner);
        match &barrier.state {
            AdmissionState::Open => return Err(AdmissionBarrierError::StalePauseSequence),
            AdmissionState::Closed { pause_seq: closed }
            | AdmissionState::Resuming {
                pause_seq: closed, ..
            } if *closed != pause_seq => {
                return Err(AdmissionBarrierError::StalePauseSequence);
            }
            AdmissionState::Resuming {
                pause_seq: resuming,
                resume_id: existing,
            } if *resuming == pause_seq && existing == resume_id => {
                if let Some(credential) = &barrier.credential {
                    return Ok(credential.clone());
                }
            }
            _ => {}
        }
        let credential = ResumeCredential {
            pause_seq,
            resume_id: resume_id.to_string(),
            token: Arc::new(()),
        };
        barrier.state = AdmissionState::Resuming {
            pause_seq,
            resume_id: resume_id.to_string(),
        };
        barrier.credential = Some(credential.clone());
        Ok(credential)
    }

    /// Reopens the root for the credential's starts; `false` when the credential is not the live one.
    pub fn release(&self, id: &str, credential: &ResumeCredential) -> bool {
        let root = self.root(id);
        let mut barrier = root.lock().unwrap_or_else(PoisonError::into_inner);
        if !matches!(barrier.state, AdmissionState::Resuming { .. })
            || barrier.credential.as_ref() != Some(credential)
        {
            return false;
        }
        barrier.state = AdmissionState::Open;
        true
    }

    /// Rebuilds a root from persisted state; a superseded or non-newest sequence stays closed.
    pub fn restore(&self, id: &str, state: AdmissionState, newest_seq: u64, superseded: bool) {
        let root = self.root(id);
        let mut barrier = root.lock().unwrap_or_else(PoisonError::into_inner);
        barrier.credential = None;
        barrier.high_water = newest_seq;
        barrier.state = match &state {
            AdmissionState::Open => AdmissionState::Open,
            AdmissionState::Closed { pause_seq } if !superseded && *pause_seq == newest_seq => {
                AdmissionState::Closed {
                    pause_seq: *pause_seq,
                }
            }
            AdmissionState::Resuming {
                pause_seq,
                resume_id,
            } if !superseded && *pause_seq == newest_seq => AdmissionState::Resuming {
                pause_seq: *pause_seq,
                resume_id: resume_id.clone(),
            },
            _ => AdmissionState::Closed {
                pause_seq: newest_seq,
            },
        };
        if let AdmissionState::Resuming {
            pause_seq,
            resume_id,
        } = &barrier.state
        {
            barrier.credential = Some(ResumeCredential {
                pause_seq: *pause_seq,
                resume_id: resume_id.clone(),
                token: Arc::new(()),
            });
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionBarrierError {
    #[error("stale pause sequence")]
    StalePauseSequence,
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn participant(key: &str) -> AdmissionParticipant {
        AdmissionParticipant {
            key: key.to_string(),
            run_ref: RunRef::task(key),
        }
    }

    fn enter(barrier: &AdmissionBarrier, root: &str, key: &str) -> AdmissionTicket {
        match barrier.enter(root, participant(key), None) {
            AdmissionEntry::Entered { ticket } => ticket,
            AdmissionEntry::Held { pause_seq } => panic!("unexpected hold at {pause_seq}"),
        }
    }

    #[test]
    fn a_closed_root_holds_every_new_start_at_the_pause_sequence() {
        let barrier = AdmissionBarrier::new();
        barrier.close("root", 4);
        match barrier.enter("root", participant("t1"), None) {
            AdmissionEntry::Held { pause_seq } => assert_eq!(pause_seq, 4),
            AdmissionEntry::Entered { .. } => panic!("a closed root must hold the start"),
        }
        assert_eq!(
            barrier.state("root"),
            AdmissionState::Closed { pause_seq: 4 }
        );
    }

    #[test]
    fn a_ticket_entered_before_the_pause_checks_held() {
        let barrier = AdmissionBarrier::new();
        let ticket = enter(&barrier, "root", "t1");
        assert_eq!(ticket.check(), AdmissionCheck::Allowed);
        let in_flight = barrier.close("root", 7);
        assert_eq!(in_flight.len(), 1);
        assert_eq!(ticket.check(), AdmissionCheck::Held { pause_seq: 7 });
        assert_eq!(
            ticket.commit(),
            AdmissionCommit::StartPaused {
                owner: RunPauseOwner {
                    parent_session_id: "root".to_string(),
                    parent_pause_seq: 7,
                    run_ref: RunRef::task("t1"),
                }
            }
        );
    }

    #[test]
    fn a_resume_credential_reopens_starts_until_it_is_released() {
        let barrier = AdmissionBarrier::new();
        barrier.close("root", 3);
        assert_eq!(
            barrier.resume("root", 2, "resume-1"),
            Err(AdmissionBarrierError::StalePauseSequence)
        );
        let credential = barrier.resume("root", 3, "resume-1").expect("credential");
        match barrier.enter("root", participant("t1"), None) {
            AdmissionEntry::Held { pause_seq } => assert_eq!(pause_seq, 3),
            AdmissionEntry::Entered { .. } => panic!("a credential is required while resuming"),
        }
        let ticket = match barrier.enter("root", participant("t2"), Some(credential.clone())) {
            AdmissionEntry::Entered { ticket } => ticket,
            AdmissionEntry::Held { pause_seq } => panic!("unexpected hold at {pause_seq}"),
        };
        assert_eq!(ticket.check(), AdmissionCheck::Allowed);
        assert!(barrier.release("root", &credential));
        assert_eq!(barrier.state("root"), AdmissionState::Open);
        assert!(!barrier.release("root", &credential));
    }

    #[test]
    fn a_superseded_restore_stays_closed_at_the_newest_sequence() {
        let barrier = AdmissionBarrier::new();
        barrier.restore(
            "root",
            AdmissionState::Resuming {
                pause_seq: 2,
                resume_id: "r".into(),
            },
            5,
            true,
        );
        assert_eq!(
            barrier.state("root"),
            AdmissionState::Closed { pause_seq: 5 }
        );
        assert_eq!(
            barrier.resume("root", 2, "r"),
            Err(AdmissionBarrierError::StalePauseSequence)
        );
    }

    #[test]
    fn restore_keeps_the_newest_closed_state() {
        let barrier = AdmissionBarrier::new();
        barrier.restore("root", AdmissionState::Closed { pause_seq: 5 }, 5, false);
        assert_eq!(
            barrier.state("root"),
            AdmissionState::Closed { pause_seq: 5 }
        );
        let credential = barrier.resume("root", 5, "r").expect("credential");
        assert_eq!(credential.pause_seq, 5);
    }

    #[tokio::test]
    async fn creations_settled_resolves_once_the_creation_leaves() {
        let barrier = AdmissionBarrier::new();
        let ticket = enter(&barrier, "root", "t1");
        ticket.creating(None);
        assert!(barrier.is_creating("root", "t1"));
        let waited = {
            let barrier = barrier.clone();
            tokio::spawn(async move { barrier.creations_settled("root").await })
        };
        ticket.leave();
        tokio::time::timeout(Duration::from_secs(5), waited)
            .await
            .expect("creations settle")
            .expect("join");
        assert!(!barrier.is_creating("root", "t1"));
        assert!(barrier.in_flight("root").is_empty());
    }

    #[tokio::test]
    async fn creations_settled_returns_immediately_when_nothing_is_creating() {
        let barrier = AdmissionBarrier::new();
        tokio::time::timeout(Duration::from_secs(5), barrier.creations_settled("root"))
            .await
            .expect("no creations");
    }
}
