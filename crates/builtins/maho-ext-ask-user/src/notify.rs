use crate::{format::status_name, schema::AskUserVariant};
use maho_ext_api::{EventBus, ExtensionContext, QuestionRequest, QuestionResponse, QuestionStatus};
use serde_json::json;

pub const ASK_USER_SETTLED_EVENT: &str = "ask-user:settled";
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
pub fn emit_notification(bus: &EventBus, request: &QuestionRequest, response: &QuestionResponse, variant: AskUserVariant) {
    if response.status == QuestionStatus::Cancelled { return; }
    bus.emit(ASK_USER_SETTLED_EVENT, &json!({"requestId":request.request_id,"status":status_name(response.status),"variant":match variant {AskUserVariant::Codex=>"codex",AskUserVariant::Claude=>"claude"}}));
}
