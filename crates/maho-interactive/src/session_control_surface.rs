//! The interactive mode's half of `TuiControlSurface` (senpi `InteractiveMode.sessionControlContext`).
//!
//! senpi's surface closures read the mode synchronously on its one event loop. The mode here is not
//! `Send`, while the endpoint serves its socket on tokio tasks, so the mode publishes what the surface
//! reads (the composer hold, the pending questions) into this shared state after every change, and
//! the surface queues what it asks the pane to do (answer a question, show a notice, switch a model,
//! set a thinking level, interrupt) for the mode to run on its next frame, answering through a
//! oneshot. The reads are the values the mode last published, which is the state a synchronous read
//! would have seen between two frames.
use crate::session_control_commands::TuiControlSurface;
use crate::session_control_session_commands::TuiSessionControls;
use futures::future::BoxFuture;
use maho_ai::types::{Model, ModelThinkingLevel};
use maho_ext_api::QuestionResponse;
use serde_json::Value;
use std::sync::{Arc, Mutex};

/// What the endpoint asks the pane to do; the mode runs it on its next frame.
pub enum ControlSurfaceAction {
    AnswerQuestion {
        request_id: String,
        response: QuestionResponse,
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Notice(String),
    SelectModel {
        model: Box<Model>,
        reply: tokio::sync::oneshot::Sender<Result<Option<String>, String>>,
    },
    SelectThinkingLevel {
        level: ModelThinkingLevel,
        remember: bool,
    },
    InterruptTurn {
        reply: tokio::sync::oneshot::Sender<()>,
    },
}

/// The values the mode publishes for the surface to read.
#[derive(Default)]
pub struct PublishedSurface {
    /// senpi `composerHold()`: `"attachment"` / `"draft"` / none.
    pub composer_hold: Option<String>,
    /// senpi `askUserQuestion !== undefined`.
    pub blocking_question: bool,
    /// Pending question ids in ask order, each with its wire `questions`.
    pub pending_questions: Vec<(String, Vec<Value>)>,
}

pub struct InteractiveControlSurface {
    published: Mutex<PublishedSurface>,
    actions: tokio::sync::mpsc::UnboundedSender<ControlSurfaceAction>,
    /// senpi `sessionControlHost.submissionInFlight()`, read with the composer hold.
    submission_in_flight: Box<dyn Fn() -> bool + Send + Sync>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl InteractiveControlSurface {
    pub fn new(
        submission_in_flight: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> (
        Arc<Self>,
        tokio::sync::mpsc::UnboundedReceiver<ControlSurfaceAction>,
    ) {
        let (actions, receiver) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(Self {
                published: Mutex::default(),
                actions,
                submission_in_flight,
            }),
            receiver,
        )
    }

    pub fn publish(&self, update: impl FnOnce(&mut PublishedSurface)) {
        update(&mut lock(&self.published));
    }

    fn request<T: Send + 'static>(
        &self,
        action: impl FnOnce(tokio::sync::oneshot::Sender<T>) -> ControlSurfaceAction,
    ) -> tokio::sync::oneshot::Receiver<T> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        drop(self.actions.send(action(reply)));
        receiver
    }
}

impl TuiSessionControls for InteractiveControlSurface {
    fn select_model(&self, model: Model) -> BoxFuture<'static, Result<Option<String>, String>> {
        let receiver = self.request(|reply| ControlSurfaceAction::SelectModel {
            model: Box::new(model),
            reply,
        });
        Box::pin(async move {
            receiver
                .await
                .unwrap_or_else(|_| Err("the terminal closed before the model switch ran".into()))
        })
    }

    fn select_thinking_level(&self, level: ModelThinkingLevel, remember: bool) {
        drop(
            self.actions
                .send(ControlSurfaceAction::SelectThinkingLevel { level, remember }),
        );
    }

    fn interrupt_turn(&self) -> BoxFuture<'static, ()> {
        let receiver = self.request(|reply| ControlSurfaceAction::InterruptTurn { reply });
        Box::pin(async move {
            drop(receiver.await);
        })
    }
}

impl TuiControlSurface for InteractiveControlSurface {
    fn draft_hold(&self) -> Option<String> {
        if (self.submission_in_flight)() {
            return Some("draft".into());
        }
        lock(&self.published).composer_hold.clone()
    }

    fn blocking_question(&self) -> bool {
        lock(&self.published).blocking_question
    }

    fn pending_question_ids(&self) -> Vec<String> {
        lock(&self.published)
            .pending_questions
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn pending_question(&self, request_id: &str) -> Option<Vec<Value>> {
        lock(&self.published)
            .pending_questions
            .iter()
            .find(|(id, _)| id == request_id)
            .map(|(_, questions)| questions.clone())
    }

    /// The mode settles the question on its next frame; this answers whether it was still pending
    /// when asked, which is what senpi's synchronous `state !== undefined` reports.
    fn answer_question(&self, request_id: &str, response: QuestionResponse) -> bool {
        let pending = {
            let mut published = lock(&self.published);
            let before = published.pending_questions.len();
            published
                .pending_questions
                .retain(|(id, _)| id != request_id);
            published.pending_questions.len() != before
        };
        if pending {
            let (reply, _settled) = tokio::sync::oneshot::channel();
            drop(self.actions.send(ControlSurfaceAction::AnswerQuestion {
                request_id: request_id.to_owned(),
                response,
                reply,
            }));
        }
        pending
    }

    fn notice(&self, line: &str) {
        drop(
            self.actions
                .send(ControlSurfaceAction::Notice(line.to_owned())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response() -> QuestionResponse {
        QuestionResponse {
            status: maho_ext_api::QuestionStatus::Answered,
            answers: Default::default(),
            comment: None,
            unanswered: Vec::new(),
            auto_resolved_after_ms: None,
            cancel_origin: None,
            resolved_by: None,
        }
    }

    #[test]
    fn a_question_is_answerable_once_and_the_answer_reaches_the_mode() {
        let (surface, mut actions) = InteractiveControlSurface::new(Box::new(|| false));
        surface.publish(|published| {
            published.pending_questions =
                vec![("r1".into(), vec![serde_json::json!({"id": "q1"})])];
        });
        assert_eq!(surface.pending_question_ids(), vec!["r1".to_owned()]);
        assert!(surface.pending_question("r1").is_some());
        assert!(surface.answer_question("r1", response()));
        assert!(!surface.answer_question("r1", response()));
        assert!(matches!(
            actions.try_recv(),
            Ok(ControlSurfaceAction::AnswerQuestion { request_id, .. }) if request_id == "r1"
        ));
        assert!(actions.try_recv().is_err());
    }

    #[test]
    fn an_open_submission_holds_admission_as_a_draft() {
        let (surface, _actions) = InteractiveControlSurface::new(Box::new(|| true));
        assert_eq!(surface.draft_hold().as_deref(), Some("draft"));
        let (surface, _actions) = InteractiveControlSurface::new(Box::new(|| false));
        assert_eq!(surface.draft_hold(), None);
        surface.publish(|published| published.composer_hold = Some("attachment".into()));
        assert_eq!(surface.draft_hold().as_deref(), Some("attachment"));
    }
}
