use maho_ext_api::{QuestionAnswer, QuestionRequest, QuestionResponse, QuestionStatus};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use tokio::sync::oneshot;
/// senpi `degradeQuestion`: a client without `question` answers each question as a select, with "Other"
/// taking a typed answer. An abort ends it (cancelled, or timed_out when the aborting side said so) and keeps
/// what was answered; a partial answer settles answered with the rest listed unanswered.
pub async fn degrade_question(
    ui: &dyn maho_ext_api::ExtensionUi,
    request: QuestionRequest,
    options: maho_ext_api::QuestionOptions,
) -> Result<QuestionResponse, maho_ext_api::ExtensionFailure> {
    let asked_at = now_ms();
    let aborted = || {
        options
            .dialog
            .signal
            .as_ref()
            .is_some_and(maho_ai::utils::abort::AbortSignal::aborted)
    };
    let mut answers = BTreeMap::new();
    let other = "Other (type an answer)";
    for question in &request.questions {
        // An abort ends the fallback: no dialog opens after it.
        if aborted() {
            break;
        }
        let labels = question
            .options
            .iter()
            .map(|option| option.label.clone())
            .chain(std::iter::once(other.into()))
            .collect::<Vec<_>>();
        let selected = ui
            .select(&question.question, &labels, options.dialog.clone())
            .await;
        // A pick that lands after the abort is not an answer, whether a label or "Other".
        if aborted() {
            break;
        }
        if selected.as_deref() == Some(other) {
            if let Some(text) = ui
                .input(&question.question, None, options.dialog.clone())
                .await
                .filter(|text| !text.trim().is_empty())
            {
                answers.insert(
                    question.id.clone(),
                    QuestionAnswer {
                        selected: vec![],
                        text: Some(text),
                    },
                );
            }
        } else if let Some(selected) = selected {
            answers.insert(
                question.id.clone(),
                QuestionAnswer {
                    selected: vec![selected],
                    text: None,
                },
            );
        }
        // Report the answers so far as the structured bridge does.
        if answers.contains_key(&question.id)
            && let Some(on_progress) = &options.on_progress
        {
            on_progress(maho_ext_api::QuestionDraft {
                answers: Some(answers.clone()),
                comment: None,
            });
        }
    }
    let unanswered = unanswered_question_ids(&request, &answers);
    if aborted() {
        let timed_out = options.dialog.signal.as_ref().is_some_and(signal_timed_out);
        return Ok(QuestionResponse {
            status: if timed_out {
                QuestionStatus::TimedOut
            } else {
                QuestionStatus::Cancelled
            },
            answers,
            comment: None,
            unanswered,
            auto_resolved_after_ms: timed_out.then(|| now_ms().saturating_sub(asked_at)),
            cancel_origin: None,
            resolved_by: None,
        });
    }
    // No trailing "anything else?" prompt: a client without `question` shows every dialog as its own card.
    let status = settled_question_status(&answers, None);
    Ok(QuestionResponse {
        status: status.unwrap_or(QuestionStatus::Cancelled),
        resolved_by: status.map(|_| maho_ext_api::QuestionResolvedBy::RpcConnection),
        answers,
        comment: None,
        unanswered,
        auto_resolved_after_ms: None,
        cancel_origin: None,
    })
}
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}
/// senpi: an aborting side that resolved `timed_out` itself passes that status as the abort reason.
pub fn signal_timed_out(signal: &maho_ai::utils::abort::AbortSignal) -> bool {
    signal
        .reason()
        .is_some_and(|reason| reason.name == "timed_out" || reason.message == "timed_out")
}
/// senpi `settledQuestionStatus`.
fn settled_question_status(
    answers: &BTreeMap<String, QuestionAnswer>,
    comment: Option<&str>,
) -> Option<QuestionStatus> {
    if comment.is_some_and(|comment| !comment.trim().is_empty()) {
        return Some(QuestionStatus::CommentSubmitted);
    }
    (!answers.is_empty()).then_some(QuestionStatus::Answered)
}
/// senpi `unansweredQuestionIds`.
fn unanswered_question_ids(
    request: &QuestionRequest,
    answers: &BTreeMap<String, QuestionAnswer>,
) -> Vec<String> {
    request
        .questions
        .iter()
        .filter(|question| {
            answers.get(&question.id).is_none_or(|answer| {
                answer.selected.is_empty()
                    && answer
                        .text
                        .as_deref()
                        .is_none_or(|text| text.trim().is_empty())
            })
        })
        .map(|question| question.id.clone())
        .collect()
}
struct Pending {
    request: QuestionRequest,
    frame: Value,
    answers: BTreeMap<String, QuestionAnswer>,
    comment: Option<String>,
    asked_at: u64,
    timeout: u64,
    sender: oneshot::Sender<QuestionResponse>,
    on_progress: Option<std::sync::Arc<dyn Fn(maho_ext_api::QuestionDraft) + Send + Sync>>,
    deliver: maho_ext_api::QuestionDelivery,
}
#[derive(Default)]
pub struct ConnectionQuestionBridge {
    pending: BTreeMap<String, Pending>,
    resolved: BTreeSet<String>,
    /// Told when a question appears or ends (senpi `changed?: (change?: UiRequestChange) => void`).
    changed: Option<crate::session_extension_ui_requests::UiRequestChanged>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum QuestionReply {
    Accepted,
    Incomplete,
    AlreadyResolved,
    Unknown,
}
fn answers_json(answers: &BTreeMap<String, QuestionAnswer>) -> Value {
    Value::Object(
        answers
            .iter()
            .map(|(id, answer)| {
                (
                    id.clone(),
                    json!({"selected":answer.selected,"text":answer.text}),
                )
            })
            .collect(),
    )
}
fn parse_answers(value: &Value) -> BTreeMap<String, QuestionAnswer> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .map(|(id, answer)| {
            (
                id.clone(),
                QuestionAnswer {
                    selected: answer
                        .get("selected")
                        .and_then(Value::as_array)
                        .map(|selected| {
                            selected
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default(),
                    text: answer
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                },
            )
        })
        .collect()
}
impl ConnectionQuestionBridge {
    /// A bridge that reports each question it registers and settles.
    pub fn with_changed(changed: crate::session_extension_ui_requests::UiRequestChanged) -> Self {
        Self {
            changed: Some(changed),
            ..Self::default()
        }
    }
    /// Installs the change listener after construction (a hosted session's binding learns its routing id late).
    pub fn set_changed(&mut self, changed: crate::session_extension_ui_requests::UiRequestChanged) {
        self.changed = Some(changed);
    }
    fn notify(&self, change: crate::session_extension_ui_requests::UiRequestChange) {
        if let Some(changed) = &self.changed {
            changed(Some(change));
        }
    }
    pub fn ask(
        &mut self,
        request: QuestionRequest,
        timeout: Option<u64>,
        now: u64,
    ) -> (Value, oneshot::Receiver<QuestionResponse>) {
        self.ask_with(
            request,
            timeout,
            None,
            maho_ext_api::QuestionDelivery::ToolResult,
            now,
        )
    }
    /// senpi `ask(request, opts)`: `on_progress` hears every draft, and `deliver` decides whether an answer is
    /// reported as a person's input here (a tool-result answer) or by the user message that carries it.
    pub fn ask_with(
        &mut self,
        request: QuestionRequest,
        timeout: Option<u64>,
        on_progress: Option<std::sync::Arc<dyn Fn(maho_ext_api::QuestionDraft) + Send + Sync>>,
        deliver: maho_ext_api::QuestionDelivery,
        now: u64,
    ) -> (Value, oneshot::Receiver<QuestionResponse>) {
        let id = uuid::Uuid::new_v4().to_string();
        let timeout = timeout.unwrap_or(request.timeout_ms);
        let questions=request.questions.iter().map(|question|json!({"id":question.id,"header":question.header,"question":question.question,"multiSelect":question.multi_select,"options":question.options.iter().map(|option|json!({"label":option.label,"description":option.description})).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let frame = json!({"type":"extension_ui_request","method":"question","id":id,"requestId":request.request_id,"toolCallId":request.request_id,"waitForAnswer":request.wait_for_answer,"questions":questions,"timeout":timeout,"askedAtMs":now,"deadlineAtMs":now+timeout,"remainingMs":timeout});
        let (sender, receiver) = oneshot::channel();
        self.notify(
            crate::session_extension_ui_requests::UiRequestChange::Pending {
                ui_request_id: id.clone(),
                method: "question".into(),
                request: frame.clone(),
            },
        );
        self.pending.insert(
            id,
            Pending {
                request,
                frame: frame.clone(),
                answers: BTreeMap::new(),
                comment: None,
                asked_at: now,
                timeout,
                sender,
                on_progress,
                deliver,
            },
        );
        (frame, receiver)
    }
    pub fn pending_questions(&self, now: u64) -> Vec<Value> {
        self.pending
            .values()
            .map(|pending| {
                let mut frame = pending.frame.clone();
                frame["remainingMs"] = frame["deadlineAtMs"]
                    .as_u64()
                    .unwrap_or(now)
                    .saturating_sub(now)
                    .into();
                frame
            })
            .collect()
    }
    /// The pending question's current idle deadline; `None` once it settled.
    pub fn deadline(&self, id: &str) -> Option<u64> {
        self.pending.get(id)?.frame["deadlineAtMs"].as_u64()
    }
    /// The idle timer fired: times the question out unless a draft re-armed it past `deadline` meanwhile.
    pub fn time_out(&mut self, id: &str, deadline: u64, now: u64) -> Option<Value> {
        (self.deadline(id)? == deadline)
            .then(|| self.finish(id, QuestionStatus::TimedOut, None, now))
            .flatten()
    }
    /// senpi `cancel`: an aborted dialog is a lifecycle cancellation, or `timed_out` when the aborting side
    /// resolved that status itself.
    pub fn abort(&mut self, id: &str, timed_out: bool, now: u64) -> Option<Value> {
        let status = if timed_out {
            QuestionStatus::TimedOut
        } else {
            QuestionStatus::Cancelled
        };
        self.finish(id, status, None, now)
    }
    fn finish(
        &mut self,
        id: &str,
        status: QuestionStatus,
        cancel_origin: Option<maho_ext_api::QuestionCancelOrigin>,
        now: u64,
    ) -> Option<Value> {
        let pending = self.pending.remove(id)?;
        self.resolved.insert(id.into());
        self.notify(
            crate::session_extension_ui_requests::UiRequestChange::Settled {
                ui_request_id: id.to_owned(),
                disposition: if matches!(
                    status,
                    QuestionStatus::Answered | QuestionStatus::CommentSubmitted
                ) {
                    "answered"
                } else {
                    "cancelled"
                },
            },
        );
        let unanswered = unanswered_question_ids(&pending.request, &pending.answers);
        let outcome = match status {
            QuestionStatus::Answered => "answered",
            QuestionStatus::CommentSubmitted => "comment-submitted",
            QuestionStatus::TimedOut => "timed_out",
            QuestionStatus::Cancelled => "cancelled",
            QuestionStatus::OrphanedAfterRestart => "orphaned_after_restart",
            QuestionStatus::Unavailable => "unavailable",
        };
        let mut record = json!({"type":"question_resolved","id":id,"requestId":pending.request.request_id,"toolCallId":pending.request.request_id,"outcome":outcome,"answers":answers_json(&pending.answers),"comment":pending.comment,"unanswered":unanswered,"deadlineAtMs":pending.frame["deadlineAtMs"]});
        if status == QuestionStatus::Answered || status == QuestionStatus::CommentSubmitted {
            record["resolvedBy"] = "rpc_connection".into();
        }
        let result = QuestionResponse {
            status,
            answers: pending.answers,
            comment: pending.comment,
            unanswered,
            cancel_origin,
            resolved_by: matches!(
                status,
                QuestionStatus::Answered | QuestionStatus::CommentSubmitted
            )
            .then_some(maho_ext_api::QuestionResolvedBy::RpcConnection),
            auto_resolved_after_ms: (status == QuestionStatus::TimedOut)
                .then_some(now.saturating_sub(pending.asked_at)),
        };
        let _ = pending.sender.send(result);
        Some(record)
    }
    pub fn respond(&mut self, response: &Value, now: u64) -> (QuestionReply, Option<Value>) {
        self.respond_with(response, now, || {})
    }
    /// senpi `respond(response, beforeAnswer)`: `before_answer` runs just before a tool-result answer settles,
    /// so the answer is reported as a person's input; an async or resumed answer is reported by its user message.
    pub fn respond_with(
        &mut self,
        response: &Value,
        now: u64,
        before_answer: impl FnOnce(),
    ) -> (QuestionReply, Option<Value>) {
        let Some(id) = response.get("id").and_then(Value::as_str) else {
            return (QuestionReply::Unknown, None);
        };
        if !self.pending.contains_key(id) {
            return (
                if self.resolved.contains(id) {
                    QuestionReply::AlreadyResolved
                } else {
                    QuestionReply::Unknown
                },
                None,
            );
        }
        // The client's cancel is the user's; a closed connection or an abort is a lifecycle cancel.
        if response.get("cancelled").is_some() {
            return (
                QuestionReply::Accepted,
                self.finish(
                    id,
                    QuestionStatus::Cancelled,
                    Some(maho_ext_api::QuestionCancelOrigin::User),
                    now,
                ),
            );
        }
        let Some(answers) = response.get("answers").cloned() else {
            return (QuestionReply::Incomplete, None);
        };
        let pending = self.pending.get_mut(id).expect("pending question exists");
        pending.answers = parse_answers(&answers);
        pending.comment = response
            .get("comment")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(status) = settled_question_status(&pending.answers, pending.comment.as_deref())
        else {
            return (QuestionReply::Incomplete, None);
        };
        if pending.request.wait_for_answer
            && pending.deliver != maho_ext_api::QuestionDelivery::UserMessage
        {
            before_answer();
        }
        (QuestionReply::Accepted, self.finish(id, status, None, now))
    }
    pub fn progress(&mut self, draft: &Value, now: u64) -> Option<Value> {
        let id = draft.get("id")?.as_str()?;
        let pending = self.pending.get_mut(id)?;
        if let Some(answers) = draft.get("answers") {
            pending.answers = parse_answers(answers);
        }
        if let Some(comment) = draft.get("comment").and_then(Value::as_str) {
            pending.comment = Some(comment.into());
        }
        pending.frame["deadlineAtMs"] = (now + pending.timeout).into();
        if let Some(on_progress) = &pending.on_progress {
            on_progress(maho_ext_api::QuestionDraft {
                answers: Some(pending.answers.clone()),
                comment: pending.comment.clone(),
            });
        }
        Some(
            json!({"type":"question_updated","id":id,"deadlineAtMs":now+pending.timeout,"remainingMs":pending.timeout}),
        )
    }
    pub fn expire(&mut self, now: u64) -> Vec<Value> {
        let ids = self
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending.frame["deadlineAtMs"]
                    .as_u64()
                    .is_some_and(|deadline| now >= deadline)
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.iter()
            .filter_map(|id| self.finish(id, QuestionStatus::TimedOut, None, now))
            .collect()
    }
    pub fn cancel_all(&mut self, now: u64) -> Vec<Value> {
        let ids = self.pending.keys().cloned().collect::<Vec<_>>();
        ids.iter()
            .filter_map(|id| self.finish(id, QuestionStatus::Cancelled, None, now))
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> QuestionRequest {
        QuestionRequest {
            request_id: "tool".into(),
            questions: vec![maho_ext_api::Question {
                id: "q".into(),
                header: "h".into(),
                question: "prompt".into(),
                options: vec![],
                multi_select: false,
            }],
            wait_for_answer: true,
            timeout_ms: 100,
        }
    }
    #[tokio::test]
    async fn typed_extension_request_settles_from_rpc_response() {
        let mut bridge = ConnectionQuestionBridge::default();
        let (frame, receiver) = bridge.ask(request(), None, 0);
        let id = frame["id"].as_str().unwrap();
        assert_eq!(
            bridge.respond(&json!({"id":id,"answers":{}}), 1).0,
            QuestionReply::Incomplete
        );
        let (reply, record) =
            bridge.respond(&json!({"id":id,"answers":{"q":{"selected":["yes"]}}}), 2);
        assert_eq!(reply, QuestionReply::Accepted);
        assert_eq!(record.unwrap()["resolvedBy"], "rpc_connection");
        assert_eq!(receiver.await.unwrap().status, QuestionStatus::Answered);
        assert_eq!(
            bridge.respond(&json!({"id":id}), 3).0,
            QuestionReply::AlreadyResolved
        );
    }
    #[test]
    fn changes_report_the_question_pending_then_answered_or_cancelled() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&seen);
        let mut bridge = ConnectionQuestionBridge::with_changed(Box::new(move |change| {
            sink.lock()
                .unwrap()
                .push(change.map(|change| change.record()));
        }));
        let (frame, _answered) = bridge.ask(request(), None, 0);
        let id = frame["id"].as_str().unwrap().to_owned();
        bridge.respond(&json!({"id":id,"answers":{"q":{"selected":["yes"]}}}), 1);
        let (other, _cancelled) = bridge.ask(request(), None, 2);
        bridge.cancel_all(3);
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].as_ref().unwrap()["type"], "ui_request_pending");
        assert_eq!(seen[0].as_ref().unwrap()["method"], "question");
        assert_eq!(seen[1].as_ref().unwrap()["disposition"], "answered");
        assert_eq!(seen[3].as_ref().unwrap()["uiRequestId"], other["id"]);
        assert_eq!(seen[3].as_ref().unwrap()["disposition"], "cancelled");
    }
    #[test]
    fn rpc_answer_resolution_record_names_resolver() {
        let mut bridge = ConnectionQuestionBridge::default();
        let (frame, _receiver) = bridge.ask(request(), None, 0);
        let id = frame["id"].as_str().unwrap();
        let (reply, record) =
            bridge.respond(&json!({"id":id,"answers":{"q":{"selected":["yes"]}}}), 1);
        assert_eq!(reply, QuestionReply::Accepted);
        assert_eq!(record.unwrap()["resolvedBy"], "rpc_connection");
    }
    /// senpi `respond(response, beforeAnswer)`: a tool-result answer is reported as a person's input, an answer
    /// delivered as a user message is not, and a comment alone never settles a question.
    #[test]
    fn only_a_tool_result_answer_runs_before_answer() {
        let mut bridge = ConnectionQuestionBridge::default();
        let (frame, _receiver) = bridge.ask(request(), None, 0);
        let id = frame["id"].as_str().unwrap();
        let mut reported = 0;
        assert_eq!(
            bridge
                .respond_with(&json!({"id":id,"comment":"only"}), 1, || reported += 1)
                .0,
            QuestionReply::Incomplete
        );
        bridge.respond_with(
            &json!({"id":id,"answers":{"q":{"selected":["yes"]}}}),
            2,
            || reported += 1,
        );
        assert_eq!(reported, 1);
        let (frame, _receiver) = bridge.ask_with(
            request(),
            None,
            None,
            maho_ext_api::QuestionDelivery::UserMessage,
            3,
        );
        let id = frame["id"].as_str().unwrap();
        bridge.respond_with(
            &json!({"id":id,"answers":{"q":{"selected":["yes"]}}}),
            4,
            || reported += 1,
        );
        assert_eq!(reported, 1);
    }
    /// senpi: the client's cancel is the user's; an abort is a lifecycle cancel, or `timed_out` when the
    /// aborting side resolved it; a stale idle timer never times out a question a draft re-armed.
    #[tokio::test]
    async fn an_abort_is_a_lifecycle_cancel_and_a_stale_timer_is_ignored() {
        let mut bridge = ConnectionQuestionBridge::default();
        let (frame, user) = bridge.ask(request(), None, 0);
        bridge.respond(&json!({"id":frame["id"],"cancelled":true}), 1);
        assert_eq!(
            user.await.unwrap().cancel_origin,
            Some(maho_ext_api::QuestionCancelOrigin::User)
        );
        let (frame, lifecycle) = bridge.ask(request(), None, 0);
        let id = frame["id"].as_str().unwrap().to_owned();
        let record = bridge.abort(&id, false, 5).unwrap();
        assert_eq!(record["outcome"], "cancelled");
        assert_eq!(lifecycle.await.unwrap().cancel_origin, None);
        let (frame, timed) = bridge.ask(request(), None, 0);
        let id = frame["id"].as_str().unwrap().to_owned();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let drafts = std::sync::Arc::clone(&seen);
        let (frame, drafted) = bridge.ask_with(
            request(),
            None,
            Some(std::sync::Arc::new(
                move |draft: maho_ext_api::QuestionDraft| {
                    drafts
                        .lock()
                        .unwrap()
                        .push(draft.answers.unwrap_or_default().len());
                },
            )),
            maho_ext_api::QuestionDelivery::ToolResult,
            0,
        );
        let drafted_id = frame["id"].as_str().unwrap().to_owned();
        bridge.progress(
            &json!({"id":drafted_id,"answers":{"q":{"selected":["a"]}}}),
            50,
        );
        assert_eq!(*seen.lock().unwrap(), vec![1]);
        assert!(bridge.time_out(&drafted_id, 100, 100).is_none());
        assert_eq!(bridge.deadline(&drafted_id), Some(150));
        assert!(bridge.abort(&id, true, 100).is_some());
        let timed = timed.await.unwrap();
        assert_eq!(timed.status, QuestionStatus::TimedOut);
        assert_eq!(timed.auto_resolved_after_ms, Some(100));
        assert!(bridge.time_out(&drafted_id, 150, 150).is_some());
        assert_eq!(drafted.await.unwrap().status, QuestionStatus::TimedOut);
    }
    #[tokio::test]
    async fn progress_rearms_idle_timeout_and_retains_draft() {
        let mut bridge = ConnectionQuestionBridge::default();
        let (frame, receiver) = bridge.ask(request(), None, 0);
        bridge.progress(
            &json!({"id":frame["id"],"answers":{"q":{"selected":[],"text":"draft"}}}),
            99,
        );
        assert!(bridge.expire(100).is_empty());
        assert_eq!(bridge.pending_questions(100)[0]["remainingMs"], 99);
        assert_eq!(bridge.expire(199).len(), 1);
        let response = receiver.await.unwrap();
        assert_eq!(response.status, QuestionStatus::TimedOut);
        assert!(response.unanswered.is_empty());
        assert_eq!(response.auto_resolved_after_ms, Some(199));
    }
}
