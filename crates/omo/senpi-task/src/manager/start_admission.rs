//! Admission-gated starts: every start enters the barrier before anything is created, and a pause
//! that lands mid-start puts the start in held custody instead of racing it
//! (`manager/start-admission.ts`).

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crate::pause::admission_barrier::{
    AdmissionBarrier, AdmissionCheck, AdmissionEntry, AdmissionParticipant, AdmissionTicket,
    ResumeCredential,
};
use crate::pause::ports::{RunRef, RunRefKind};

use super::types::ManagerStartSpec;

/// A start that landed in held custody: nothing was created, and the pause owner holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldStart {
    pub kind: &'static str,
    pub pause_seq: u64,
    pub root_session_id: String,
    pub task_id: String,
}

pub const HELD_START_KIND: &str = "held";

impl HeldStart {
    fn new(pause_seq: u64, task_id: &str, root_session_id: &str) -> Self {
        Self {
            kind: HELD_START_KIND,
            pause_seq,
            root_session_id: root_session_id.to_string(),
            task_id: task_id.to_string(),
        }
    }
}

/// Raised by [`StartAdmission::assert_open`] when the pause landed after the start entered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("run admission is held")]
pub struct AdmissionHeld {
    pub result: HeldStart,
}

/// The held-start custody carried by an error, when the error is an [`AdmissionHeld`].
pub fn admission_held_result(error: &(dyn std::error::Error + 'static)) -> Option<HeldStart> {
    error
        .downcast_ref::<AdmissionHeld>()
        .map(|held| held.result.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StartAdmissionError {
    #[error("open admission cannot enter held custody")]
    OpenAdmissionNotHeld,
}

/// The custody callback the manager uses to persist a held start.
pub type HeldCallback = Arc<dyn Fn(&HeldStart, &ManagerStartSpec) + Send + Sync>;

/// The admission scope one start owns: its ticket, the task id it will create, and its run ref.
#[derive(Clone)]
pub struct StartAdmission {
    ticket: AdmissionTicket,
    task_id: Arc<Mutex<String>>,
    root_session_id: String,
    run_ref: Arc<Mutex<RunRef>>,
    transferred: Arc<AtomicBool>,
    held: Option<HeldCallback>,
    spec: Arc<ManagerStartSpec>,
}

impl StartAdmission {
    pub fn task_id(&self) -> String {
        self.task_id
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn root_session_id(&self) -> &str {
        &self.root_session_id
    }

    pub fn run_ref(&self) -> RunRef {
        self.run_ref
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Declares that this start is about to create `task_id`.
    pub fn creating(&self) {
        let task_id = self.task_id();
        self.ticket.creating(Some(&task_id));
    }

    pub fn update(&self, participant: AdmissionParticipant) {
        self.ticket.update(participant);
    }

    pub fn check(&self) -> AdmissionCheck {
        self.ticket.check()
    }

    /// Binds the real task id once it exists; a task-kind run ref follows it.
    pub fn bind(&self, task_id: &str) {
        *self.task_id.lock().unwrap_or_else(PoisonError::into_inner) = task_id.to_string();
        let next = {
            let mut run_ref = self.run_ref.lock().unwrap_or_else(PoisonError::into_inner);
            if matches!(run_ref.kind, RunRefKind::Task) {
                *run_ref = RunRef::task(task_id);
            }
            run_ref.clone()
        };
        self.ticket.update(AdmissionParticipant {
            key: task_id.to_string(),
            run_ref: next,
        });
    }

    /// Marks the launch as handed to the runner, so the caller must not leave the ticket.
    pub fn transfer(&self) {
        self.transferred.store(true, Ordering::SeqCst);
    }

    pub fn transferred(&self) -> bool {
        self.transferred.load(Ordering::SeqCst)
    }

    /// Moves an entered start into held custody: the pause owner keeps the ticket open.
    pub fn hold(&self) -> Result<HeldStart, StartAdmissionError> {
        let pause_seq = match self.ticket.check() {
            AdmissionCheck::Held { pause_seq } => pause_seq,
            AdmissionCheck::Allowed => return Err(StartAdmissionError::OpenAdmissionNotHeld),
        };
        let custody = HeldStart::new(pause_seq, &self.task_id(), &self.root_session_id);
        if let Some(held) = &self.held {
            held(&custody, &self.spec);
        }
        Ok(custody)
    }

    /// Fails once the pause landed: a start may not create anything after that.
    pub fn assert_open(&self) -> Result<(), AdmissionHeld> {
        match self.ticket.check() {
            AdmissionCheck::Allowed => Ok(()),
            AdmissionCheck::Held { pause_seq } => Err(AdmissionHeld {
                result: HeldStart::new(pause_seq, &self.task_id(), &self.root_session_id),
            }),
        }
    }

    pub fn leave(&self) {
        self.ticket.leave();
    }
}

pub struct EnterStartAdmissionInput<'a> {
    pub barrier: &'a AdmissionBarrier,
    pub spec: &'a ManagerStartSpec,
    pub task_id: String,
    pub run_ref: Option<RunRef>,
    pub credential: Option<ResumeCredential>,
    pub held: Option<HeldCallback>,
}

pub struct EnteredStart {
    pub scope: StartAdmission,
    pub spec: ManagerStartSpec,
}

pub enum StartAdmissionEntry {
    Held(HeldStart),
    Entered(Box<EnteredStart>),
}

/// Enters the root's barrier for one start; a held result means nothing may be created.
pub fn enter_start_admission(input: EnterStartAdmissionInput<'_>) -> StartAdmissionEntry {
    let EnterStartAdmissionInput {
        barrier,
        spec,
        task_id,
        run_ref,
        credential,
        held,
    } = input;
    let root_session_id = spec
        .root_session_id
        .clone()
        .unwrap_or_else(|| spec.parent_session_id.clone());
    let run_ref = run_ref.unwrap_or_else(|| RunRef::task(task_id.clone()));
    let participant = AdmissionParticipant {
        key: task_id.clone(),
        run_ref: run_ref.clone(),
    };
    match barrier.enter(&root_session_id, participant, credential) {
        AdmissionEntry::Held { pause_seq } => {
            let custody = HeldStart::new(pause_seq, &task_id, &root_session_id);
            if let Some(held) = &held {
                held(&custody, spec);
            }
            StartAdmissionEntry::Held(custody)
        }
        AdmissionEntry::Entered { ticket } => {
            let scope = StartAdmission {
                ticket,
                task_id: Arc::new(Mutex::new(task_id)),
                root_session_id,
                run_ref: Arc::new(Mutex::new(run_ref)),
                transferred: Arc::new(AtomicBool::new(false)),
                held,
                spec: Arc::new(spec.clone()),
            };
            let mut spec = spec.clone();
            spec.admission = Some(scope.clone());
            StartAdmissionEntry::Entered(Box::new(EnteredStart { scope, spec }))
        }
    }
}

/// One admission-gated start's outcome: the body's value, or the custody the pause took.
pub enum StartOutcome<T> {
    Done(T),
    Held(HeldStart),
}

/// Creates admission scopes for a manager: one barrier shared by every start of a root session.
#[derive(Clone)]
pub struct StartAdmissions {
    barrier: AdmissionBarrier,
    held: Option<HeldCallback>,
    new_task_id: Arc<dyn Fn() -> String + Send + Sync>,
}

impl StartAdmissions {
    pub fn new(
        barrier: AdmissionBarrier,
        held: Option<HeldCallback>,
        new_task_id: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        Self {
            barrier,
            held,
            new_task_id,
        }
    }

    pub fn barrier(&self) -> &AdmissionBarrier {
        &self.barrier
    }

    /// Runs `body` inside one admission scope; the ticket is left unless the body transferred it.
    /// A body that fails with an [`AdmissionHeld`] lands in held custody; every other error
    /// propagates unchanged.
    pub async fn run<T, E, F, Fut>(
        &self,
        spec: &ManagerStartSpec,
        run_ref: Option<RunRef>,
        credential: Option<ResumeCredential>,
        body: F,
    ) -> Result<StartOutcome<T>, E>
    where
        E: std::error::Error + 'static,
        F: FnOnce(ManagerStartSpec, StartAdmission) -> Fut,
        Fut: Future<Output = Result<T, E>>,
    {
        let entered = enter_start_admission(EnterStartAdmissionInput {
            barrier: &self.barrier,
            spec,
            task_id: (self.new_task_id)(),
            run_ref,
            credential: credential.or_else(|| spec.resume_credential.clone()),
            held: self.held.clone(),
        });
        let EnteredStart { scope, spec } = match entered {
            StartAdmissionEntry::Held(custody) => return Ok(StartOutcome::Held(custody)),
            StartAdmissionEntry::Entered(entered) => *entered,
        };
        let outcome = body(spec, scope.clone()).await;
        let transferred = scope.transferred();
        if !transferred {
            scope.leave();
        }
        match outcome {
            Ok(value) => Ok(StartOutcome::Done(value)),
            Err(error) => {
                match (&error as &dyn std::error::Error).downcast_ref::<AdmissionHeld>() {
                    Some(held) => {
                        let custody = held.result.clone();
                        if let Some(callback) = &self.held {
                            callback(&custody, &scope.spec);
                        }
                        Ok(StartOutcome::Held(custody))
                    }
                    None => Err(error),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use super::*;
    use crate::pause::admission_barrier::AdmissionBarrier;

    fn spec() -> ManagerStartSpec {
        ManagerStartSpec {
            prompt: "do it".to_string(),
            parent_session_id: "parent".to_string(),
            root_session_id: Some("root".to_string()),
            ..ManagerStartSpec::default()
        }
    }

    fn held_log() -> (HeldCallback, Arc<Mutex<Vec<HeldStart>>>) {
        let log: Arc<Mutex<Vec<HeldStart>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&log);
        let callback: HeldCallback = Arc::new(move |held: &HeldStart, _spec: &ManagerStartSpec| {
            sink.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(held.clone());
        });
        (callback, log)
    }

    fn entered(barrier: &AdmissionBarrier) -> StartAdmission {
        match enter_start_admission(EnterStartAdmissionInput {
            barrier,
            spec: &spec(),
            task_id: "task-1".to_string(),
            run_ref: None,
            credential: None,
            held: None,
        }) {
            StartAdmissionEntry::Entered(entered) => entered.scope,
            StartAdmissionEntry::Held(_) => panic!("an open root must enter"),
        }
    }

    #[test]
    fn a_paused_root_holds_the_start_before_anything_is_created() {
        let barrier = AdmissionBarrier::new();
        barrier.close("root", 2);
        let (callback, log) = held_log();
        let custody = match enter_start_admission(EnterStartAdmissionInput {
            barrier: &barrier,
            spec: &spec(),
            task_id: "task-1".to_string(),
            run_ref: None,
            credential: None,
            held: Some(callback),
        }) {
            StartAdmissionEntry::Held(custody) => custody,
            StartAdmissionEntry::Entered(_) => panic!("expected held custody"),
        };
        assert_eq!(
            custody,
            HeldStart {
                kind: "held",
                pause_seq: 2,
                root_session_id: "root".to_string(),
                task_id: "task-1".to_string(),
            }
        );
        assert_eq!(log.lock().unwrap_or_else(PoisonError::into_inner).len(), 1);
        assert!(barrier.in_flight("root").is_empty());
    }

    #[test]
    fn binding_a_start_moves_its_task_id_and_task_run_ref() {
        let barrier = AdmissionBarrier::new();
        let scope = entered(&barrier);
        assert_eq!(scope.task_id(), "task-1");
        assert_eq!(scope.run_ref(), RunRef::task("task-1"));
        scope.bind("st_123");
        assert_eq!(scope.task_id(), "st_123");
        assert_eq!(scope.run_ref(), RunRef::task("st_123"));
        assert_eq!(
            barrier.in_flight("root"),
            vec![AdmissionParticipant {
                key: "st_123".to_string(),
                run_ref: RunRef::task("st_123"),
            }]
        );
        assert!(!scope.transferred());
        scope.transfer();
        assert!(scope.transferred());
        scope.leave();
        assert!(barrier.in_flight("root").is_empty());
    }

    #[test]
    fn assert_open_fails_with_the_custody_once_the_pause_lands() {
        let barrier = AdmissionBarrier::new();
        let scope = entered(&barrier);
        assert!(scope.assert_open().is_ok());
        barrier.close("root", 9);
        let held = scope.assert_open().expect_err("held");
        assert_eq!(held.result.pause_seq, 9);
        assert_eq!(held.result.task_id, "task-1");
        assert_eq!(held.result.root_session_id, "root");
        assert_eq!(held.to_string(), "run admission is held");
        assert_eq!(
            admission_held_result(&held).map(|custody| custody.pause_seq),
            Some(9)
        );
    }

    #[test]
    fn an_open_admission_cannot_enter_held_custody() {
        let barrier = AdmissionBarrier::new();
        let scope = entered(&barrier);
        assert_eq!(scope.hold(), Err(StartAdmissionError::OpenAdmissionNotHeld));
    }

    #[tokio::test]
    async fn run_leaves_the_ticket_when_the_body_does_not_transfer() {
        let barrier = AdmissionBarrier::new();
        let admissions =
            StartAdmissions::new(barrier.clone(), None, Arc::new(|| "task-1".to_string()));
        let outcome = admissions
            .run(&spec(), None, None, |_admitted, scope| async move {
                assert_eq!(scope.task_id(), "task-1");
                Ok::<String, AdmissionHeld>("done".to_string())
            })
            .await
            .expect("no error");
        match outcome {
            StartOutcome::Done(value) => assert_eq!(value, "done"),
            StartOutcome::Held(_) => panic!("expected done"),
        }
        assert!(barrier.in_flight("root").is_empty());
    }

    #[tokio::test]
    async fn run_reports_held_custody_when_the_body_is_paused_mid_start() {
        let barrier = AdmissionBarrier::new();
        let (callback, log) = held_log();
        let admissions = StartAdmissions::new(
            barrier.clone(),
            Some(callback),
            Arc::new(|| "task-1".to_string()),
        );
        let outcome = admissions
            .run(&spec(), None, None, |_admitted, scope| async move {
                let _ = scope;
                Err::<String, AdmissionHeld>(AdmissionHeld {
                    result: HeldStart {
                        kind: HELD_START_KIND,
                        pause_seq: 5,
                        root_session_id: "root".to_string(),
                        task_id: "task-1".to_string(),
                    },
                })
            })
            .await
            .expect("held custody is not an error");
        match outcome {
            StartOutcome::Held(custody) => assert_eq!(custody.pause_seq, 5),
            StartOutcome::Done(_) => panic!("expected held custody"),
        }
        assert_eq!(log.lock().unwrap_or_else(PoisonError::into_inner).len(), 1);
        assert!(barrier.in_flight("root").is_empty());
    }
}
