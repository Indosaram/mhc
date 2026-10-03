use crate::schema::AskUserVariant;
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
pub fn emit_asked(bus: &EventBus, ctx: &ExtensionContext, request: &QuestionRequest, variant: AskUserVariant) {
    bus.emit(ASK_USER_ASKED_EVENT, &json!({"request":{"requestId":request.request_id,"waitForAnswer":request.wait_for_answer,"timeoutMs":request.timeout_ms,"questions":request.questions.iter().map(|question|json!({"id":question.id,"header":question.header,"question":question.question,"multiSelect":question.multi_select,"options":question.options.iter().map(|option|json!({"label":option.label,"description":option.description})).collect::<Vec<_>>()})).collect::<Vec<_>>()},"variant":match variant {AskUserVariant::Codex=>"codex",AskUserVariant::Claude=>"claude"}}));
    bus.emit_native(ASK_USER_ASKED_EVENT, &AskUserAskedEvent { ctx: ctx.clone(), request: request.clone(), variant });
}
pub fn emit_notification(bus: &EventBus, ctx: &ExtensionContext, request: &QuestionRequest, response: &QuestionResponse, variant: AskUserVariant) {
    if response.status == QuestionStatus::Cancelled { return; }
    bus.emit_native(ASK_USER_SETTLED_EVENT, &AskUserSettledEvent { ctx: ctx.clone(), request: request.clone(), response: response.clone(), variant });
}
