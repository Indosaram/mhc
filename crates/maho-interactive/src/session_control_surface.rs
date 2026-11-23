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

mod publication;
use publication::Publication;

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
    published: Publication,
    track_delivery: bool,
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
        Self::with_delivery(submission_in_flight, false)
    }

    pub(crate) fn new_for_mode(
        submission_in_flight: Box<dyn Fn() -> bool + Send + Sync>,
    ) -> (
        Arc<Self>,
        tokio::sync::mpsc::UnboundedReceiver<ControlSurfaceAction>,
    ) {
        Self::with_delivery(submission_in_flight, true)
    }

    fn with_delivery(
        submission_in_flight: Box<dyn Fn() -> bool + Send + Sync>,
        track_delivery: bool,
    ) -> (
        Arc<Self>,
        tokio::sync::mpsc::UnboundedReceiver<ControlSurfaceAction>,
    ) {
        let (actions, receiver) = tokio::sync::mpsc::unbounded_channel();
        (
            Arc::new(Self {
                published: Publication::default(),
                track_delivery,
                actions,
                submission_in_flight,
            }),
            receiver,
        )
    }

    pub fn publish(&self, update: impl FnOnce(&mut PublishedSurface)) {
        self.published.publish(update);
    }

    pub(crate) fn publish_inputs(&self, draft: bool, attachment: bool, blocking: bool) {
        let mut state = self.published.write();
        let hold = if attachment {
            Some("attachment")
        } else {
            draft.then_some("draft")
        };
        if state.value.composer_hold.as_deref() != hold {
            state.value.composer_hold = hold.map(str::to_owned);
        }
        state.value.blocking_question = blocking;
    }

    pub(crate) fn attach_question(
        &self,
        request: &crate::components::ask_user_question_state::QuestionRequest,
        order: &[String],
    ) {
        let questions = request.wire_questions();
        let mut state = self.published.write();
        #[cfg(test)]
        {
            state.projections += 1;
        }
        let retired = state.claims.remove(&request.request_id);
        let pending = &mut state.value.pending_questions;
        if let Some((_, current)) = pending.iter_mut().find(|(id, _)| id == &request.request_id) {
            *current = questions;
        } else {
            // A previously claimed reattach takes its registry position among visible rows.
            let index = order
                .iter()
                .position(|id| id == &request.request_id)
                .and_then(|i| {
                    pending
                        .iter()
                        .position(|(id, _)| order[i + 1..].contains(id))
                })
                .unwrap_or(pending.len());
            pending.insert(index, (request.request_id.clone(), questions));
        }
        drop(state);
        drop(retired);
    }

    pub(crate) fn finish_question(&self, request_id: &str) {
        let mut state = self.published.write();
        state
            .value
            .pending_questions
            .retain(|(id, _)| id != request_id);
        let retired = state.claims.remove(request_id);
        drop(state);
        drop(retired);
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
        self.published.read().value.composer_hold.clone()
    }

    fn blocking_question(&self) -> bool {
        self.published.read().value.blocking_question
    }

    fn pending_question_ids(&self) -> Vec<String> {
        self.published
            .read()
            .value
            .pending_questions
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn pending_question(&self, request_id: &str) -> Option<Vec<Value>> {
        self.published
            .read()
            .value
            .pending_questions
            .iter()
            .find(|(id, _)| id == request_id)
            .map(|(_, questions)| questions.clone())
    }

    /// The mode settles the question on its next frame; this answers whether it was still pending
    /// when asked, which is what senpi's synchronous `state !== undefined` reports.
    fn answer_question(&self, request_id: &str, response: QuestionResponse) -> bool {
        let mut state = self.published.write();
        let pending = &mut state.value.pending_questions;
        let before = pending.len();
        pending.retain(|(id, _)| id != request_id);
        if pending.len() == before {
            return false;
        }
        let (reply, settled) = tokio::sync::oneshot::channel();
        let retired = if self.track_delivery {
            state.claims.insert(request_id.to_owned(), settled)
        } else {
            None
        };
        drop(state);
        drop(retired);
        // Sending wakes the receiver inline. A failed send owns this action, not
        // the current incarnation at request_id; never remove a claim by ID here.
        drop(self.actions.send(ControlSurfaceAction::AnswerQuestion {
            request_id: request_id.to_owned(),
            response,
            reply,
        }));
        true
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
            published.pending_questions = vec![("r1".into(), vec![serde_json::json!({"id":"q1"})])];
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
        surface.publish_inputs(false, true, false);
        assert_eq!(surface.draft_hold().as_deref(), Some("draft"));
        let (surface, _actions) = InteractiveControlSurface::new(Box::new(|| false));
        assert_eq!(surface.draft_hold(), None);
        surface.publish_inputs(true, true, false);
        assert_eq!(surface.draft_hold().as_deref(), Some("attachment"));
    }

    fn question(
        id: &str,
        item: &str,
    ) -> crate::components::ask_user_question_state::QuestionRequest {
        serde_json::from_value(serde_json::json!({"requestId": id, "waitForAnswer": false,
            "timeoutMs": 0, "questions": [{"id":item,"question":"Pick","header":"H","options":[]}]})).unwrap()
    }

    #[tokio::test]
    async fn unchanged_input_publications_do_not_project_questions_or_reopen_claims() {
        use crate::{
            interactive_extension_ui::UiRequest,
            interactive_mode::InteractiveMode,
            question_registry::WIRE_PROJECTIONS,
            theme::{ColorMode, Theme},
        };
        use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
        struct Host;
        impl maho_tui::components::editor::EditorTuiHost for Host {
            fn request_render(&self) {}
            fn terminal_rows(&self) -> usize {
                32
            }
        }
        let handle = FauxSession::new(FauxScript {
            name: "projection-count".into(),
            prompt: String::new(),
            responses: vec![],
        })
        .run_native_handle()
        .await
        .expect("private session");
        let mut mode = InteractiveMode::new(
            Arc::new(handle.agent_session().clone()),
            Theme::builtin("dark", ColorMode::Truecolor).expect("theme"),
            std::rc::Rc::new(Host),
        );
        let (reply, _rx) = tokio::sync::oneshot::channel();
        let initial = WIRE_PROJECTIONS.get();
        mode.handle_ui_request(UiRequest::Question {
            request: maho_ext_api::QuestionRequest {
                request_id: "stable".into(),
                wait_for_answer: false,
                timeout_ms: 0,
                questions: vec![maho_ext_api::Question {
                    id: "q".into(),
                    header: "Q".into(),
                    question: "Pick".into(),
                    multi_select: false,
                    options: vec![],
                }],
            },
            options: Default::default(),
            reply,
        });
        let mut samples = vec![WIRE_PROJECTIONS.get()];
        for _ in 0..32 {
            mode.drain_events();
            samples.push(WIRE_PROJECTIONS.get());
        }
        assert!(samples[0] > initial && samples.iter().all(|c| *c == samples[0]));
        eprintln!(
            "RP2_OWNER {}",
            serde_json::json!({"tag": "mode-drain", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": {"initial":initial,"samples":samples}})
        );

        let (surface, _actions) = InteractiveControlSurface::new(Box::new(|| false));
        surface.attach_question(&question("a", "first"), &[]);
        surface.attach_question(&question("b", "second"), &[]);
        assert!(surface.answer_question("a", response()));
        let mut counts = vec![surface.published.read().projections];
        for _ in 0..32 {
            surface.publish_inputs(true, false, false);
            counts.push(surface.published.read().projections);
        }
        eprintln!(
            "RP2_OWNER {}",
            serde_json::json!({"tag": "drain", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": {"counts":counts,"pending":surface.pending_question_ids(),"answeredVisible":surface.pending_question("a").is_some()}})
        );
        assert_eq!(surface.published.read().projections, 2);
        assert_eq!(surface.pending_question_ids(), ["b"]);
        assert!(surface.pending_question("a").is_none());
    }

    #[test]
    fn public_reopen_cancels_only_the_answer_for_that_incarnation() {
        let (surface, mut actions) = InteractiveControlSurface::new_for_mode(Box::new(|| false));
        for id in ["a", "b"] {
            surface.attach_question(&question(id, "old"), &[]);
            assert!(surface.answer_question(id, response()));
        }
        surface.publish(|p| p.pending_questions.push(("a".into(), vec![])));
        assert!(surface.answer_question("a", response()));
        for (id, live) in [("a", false), ("b", true), ("a", true)] {
            let ControlSurfaceAction::AnswerQuestion {
                request_id, reply, ..
            } = actions.try_recv().unwrap()
            else {
                panic!("missing answer")
            };
            assert_eq!(request_id, id);
            assert_eq!(!reply.is_closed(), live);
        }
    }

    #[test]
    fn replacement_preserves_order_and_rejects_the_previous_incarnations_action() {
        let (surface, mut actions) = InteractiveControlSurface::new_for_mode(Box::new(|| false));
        surface.attach_question(&question("a", "old"), &[]);
        assert!(surface.answer_question("a", response()));
        surface.attach_question(&question("b", "other"), &[]);
        assert!(!surface.answer_question("a", response()));
        surface.attach_question(&question("a", "new"), &["a".into(), "b".into()]);
        assert_eq!(surface.pending_question_ids(), ["a", "b"]);
        eprintln!(
            "RP2_OWNER {}",
            serde_json::json!({"tag": "new-question", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": {"ids":surface.pending_question_ids(),"wire":surface.pending_question("a")}})
        );
        assert_eq!(surface.pending_question("a").unwrap()[0]["id"], "new");
        assert!(surface.answer_question("a", response()));
        assert!(!surface.answer_question("a", response()));
        for expected in [false, true] {
            let ControlSurfaceAction::AnswerQuestion { reply, .. } = actions.try_recv().unwrap()
            else {
                panic!("answer")
            };
            eprintln!(
                "RP2_OWNER {}",
                serde_json::json!({"tag": "incarnation", "argv": std::env::args().collect::<Vec<_>>(), "exe": std::env::current_exe().expect("current test executable"), "data": (!reply.is_closed())})
            );
            assert_eq!(!reply.is_closed(), expected);
        }
    }
}
