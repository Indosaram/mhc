use super::{PublishedSurface, lock};
use std::collections::BTreeMap;
use std::sync::{Condvar, Mutex, MutexGuard};
use tokio::sync::oneshot;

#[derive(Default)]
pub(super) struct State {
    pub value: PublishedSurface,
    // Receivers exist only for undelivered private-mode answers. Dropping one
    // cancels precisely its queued sender without changing the public action.
    pub claims: BTreeMap<String, oneshot::Receiver<bool>>,
    editing: bool,
    #[cfg(test)]
    pub projections: usize,
}

#[derive(Default)]
pub(super) struct Publication {
    state: Mutex<State>,
    writers: Condvar,
}

impl Publication {
    pub fn read(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub fn write(&self) -> MutexGuard<'_, State> {
        self.writers
            .wait_while(self.read(), |s| s.editing)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    // Only the public, arbitrary FnOnce needs an exclusive edit transaction.
    // Readers can see the last committed value, including from the callback;
    // another writer cannot interleave and accidentally resurrect an answer.
    pub fn publish(&self, update: impl FnOnce(&mut PublishedSurface)) {
        let mut state = self.write();
        let committed = PublishedSurface {
            composer_hold: state.value.composer_hold.clone(),
            blocking_question: state.value.blocking_question,
            pending_questions: state.value.pending_questions.clone(),
        };
        let mut edit = Edit {
            publication: self,
            value: std::mem::replace(&mut state.value, committed),
        };
        state.editing = true;
        drop(state);
        update(&mut edit.value);
    }
}

struct Edit<'a> {
    publication: &'a Publication,
    value: PublishedSurface,
}

impl Drop for Edit<'_> {
    fn drop(&mut self) {
        let mut state = self.publication.read();
        let retired: Vec<_> = self
            .value
            .pending_questions
            .iter()
            .filter_map(|(id, _)| state.claims.remove(id))
            .collect();
        state.value = std::mem::take(&mut self.value);
        state.editing = false;
        drop(state);
        self.publication.writers.notify_all();
        drop(retired);
    }
}

#[cfg(test)]
mod tests {
    use crate::session_control_commands::TuiControlSurface;
    use crate::session_control_surface::{ControlSurfaceAction, InteractiveControlSurface};
    use maho_ext_api::{QuestionResponse, QuestionStatus};
    use std::sync::Arc;
    use std::task::{Context, Wake, Waker};

    struct ObserveWake {
        surface: Arc<InteractiveControlSurface>,
        seen: std::sync::OnceLock<bool>,
    }

    impl Wake for ObserveWake {
        fn wake(self: Arc<Self>) {
            let committed =
                self.surface.published.state.try_lock().is_ok_and(|s| {
                    s.value.blocking_question && s.value.pending_questions.is_empty()
                });
            self.seen.set(committed).expect("one registered wake");
        }
    }

    #[test]
    fn answer_wakes_receiver_after_committing_and_unlocking() {
        for tracked in [false, true] {
            let (surface, mut actions) =
                InteractiveControlSurface::with_delivery(Box::new(|| false), tracked);
            surface.publish(|p| {
                p.blocking_question = true;
                p.pending_questions.push(("r".into(), vec![]));
            });
            let observer = Arc::new(ObserveWake {
                surface: Arc::clone(&surface),
                seen: Default::default(),
            });
            let waker = Waker::from(Arc::clone(&observer));
            let mut cx = Context::from_waker(&waker);
            assert!(actions.poll_recv(&mut cx).is_pending());
            let expected = QuestionResponse {
                status: QuestionStatus::Answered,
                answers: Default::default(),
                comment: Some("selected".into()),
                unanswered: vec![],
                auto_resolved_after_ms: None,
                cancel_origin: None,
                resolved_by: None,
            };

            assert!(surface.answer_question("r", expected.clone()));

            assert_eq!(observer.seen.get(), Some(&true));
            let ControlSurfaceAction::AnswerQuestion {
                request_id,
                response,
                reply,
            } = actions.try_recv().expect("answer")
            else {
                panic!("expected an answer");
            };
            assert_eq!(request_id, "r");
            assert_eq!(response, expected);
            assert_eq!(reply.send(true).is_ok(), tracked);
        }
    }
}
