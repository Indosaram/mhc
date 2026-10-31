use crate::format::status_name;
use crate::schema::AskUserVariant;
use maho_ext_api::{EventBus, ExtensionContext, QuestionRequest, QuestionResponse, QuestionStatus};
use serde_json::json;

pub const ASK_USER_SETTLED_EVENT: &str = "ask-user:settled";
pub const ASK_USER_CLOSED_EVENT: &str = "ask-user:closed";
pub const ASK_USER_ASKED_EVENT: &str = "ask-user:asked";
pub const ASK_USER_QUESTION_ENTRY: &str = "ask-user:question";
pub const ASK_USER_SETTLEMENT_ENTRY: &str = "ask-user:settlement";
#[derive(Clone)]
pub struct AskUserAskedEvent {
    pub ctx: ExtensionContext,
    pub request: QuestionRequest,
    pub variant: AskUserVariant,
}
#[derive(Clone)]
pub struct AskUserSettledEvent {
    pub ctx: ExtensionContext,
    pub request: QuestionRequest,
    pub response: QuestionResponse,
    pub variant: AskUserVariant,
}
pub fn emit_asked(
    bus: &EventBus,
    ctx: &ExtensionContext,
    request: &QuestionRequest,
    variant: AskUserVariant,
) {
    bus.emit(ASK_USER_ASKED_EVENT, &json!({"request":{"requestId":request.request_id,"waitForAnswer":request.wait_for_answer,"timeoutMs":request.timeout_ms,"questions":request.questions.iter().map(|question|json!({"id":question.id,"header":question.header,"question":question.question,"multiSelect":question.multi_select,"options":question.options.iter().map(|option|json!({"label":option.label,"description":option.description})).collect::<Vec<_>>()})).collect::<Vec<_>>()},"variant":match variant {AskUserVariant::Codex=>"codex",AskUserVariant::Claude=>"claude"}}));
    bus.emit_native(
        ASK_USER_ASKED_EVENT,
        &AskUserAskedEvent {
            ctx: ctx.clone(),
            request: request.clone(),
            variant,
        },
    );
}
/// `AskUserClosedEvent` payload: `{ requestId, status, resolvedBy? }`.
pub fn ask_user_closed_payload(request_id: &str, response: &QuestionResponse) -> serde_json::Value {
    let mut payload = json!({"requestId": request_id, "status": status_name(response.status)});
    if let Some(resolved_by) = response.resolved_by {
        payload["resolvedBy"] = json!(resolved_by.as_str());
    }
    payload
}
/// Every terminal outcome, including cancellations that remain silent to settled listeners.
pub fn emit_ask_user_closed(bus: &EventBus, request_id: &str, response: &QuestionResponse) {
    bus.emit(
        ASK_USER_CLOSED_EVENT,
        &ask_user_closed_payload(request_id, response),
    );
}
/// Publish only; the active hooks builtin owns preparation and command execution.
pub fn emit_notification(
    bus: &EventBus,
    ctx: &ExtensionContext,
    request: &QuestionRequest,
    response: &QuestionResponse,
    variant: AskUserVariant,
) {
    if response.status == QuestionStatus::Cancelled {
        return;
    }
    bus.emit_native(
        ASK_USER_SETTLED_EVENT,
        &AskUserSettledEvent {
            ctx: ctx.clone(),
            request: request.clone(),
            response: response.clone(),
            variant,
        },
    );
}
/// Every terminal outcome, including cancellations that remain silent to settled listeners.
pub fn emit_closed(bus: &EventBus, request_id: &str, response: &QuestionResponse) {
    emit_ask_user_closed(bus, request_id, response);
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::QuestionResolvedBy;
    use std::{
        collections::BTreeMap,
        sync::{Arc, Mutex},
    };

    fn response(
        status: QuestionStatus,
        resolved_by: Option<QuestionResolvedBy>,
    ) -> QuestionResponse {
        QuestionResponse {
            status,
            answers: BTreeMap::new(),
            comment: None,
            unanswered: Vec::new(),
            auto_resolved_after_ms: None,
            cancel_origin: None,
            resolved_by,
        }
    }

    #[test]
    fn closed_event_reports_every_terminal_outcome_including_cancellation() {
        let bus = EventBus::default();
        let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let recorded = seen.clone();
        let _subscription = bus.on(
            ASK_USER_CLOSED_EVENT,
            Arc::new(move |data| recorded.lock().unwrap().push(data.clone())),
        );
        emit_ask_user_closed(&bus, "r1", &response(QuestionStatus::Cancelled, None));
        emit_ask_user_closed(
            &bus,
            "r2",
            &response(
                QuestionStatus::Answered,
                Some(QuestionResolvedBy::RpcConnection),
            ),
        );
        emit_ask_user_closed(
            &bus,
            "r3",
            &response(QuestionStatus::OrphanedAfterRestart, None),
        );
        assert_eq!(
            seen.lock().unwrap().clone(),
            vec![
                json!({"requestId": "r1", "status": "cancelled"}),
                json!({"requestId": "r2", "status": "answered", "resolvedBy": "rpc_connection"}),
                json!({"requestId": "r3", "status": "orphaned-after-restart"}),
            ]
        );
    }
}
