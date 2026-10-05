use serde::{Deserialize,Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Stable machine-matchable error codes carried in response `error` fields (rpc-types.ts).
pub const RPC_ERROR_UNKNOWN_SESSION:&str="unknown_session";
pub const RPC_ERROR_SESSION_CLOSING:&str="session_closing";
pub const RPC_ERROR_SESSION_PATH_IN_USE:&str="session_path_in_use";
pub const RPC_ERROR_SESSION_RESERVATION_LIMIT:&str="session_reservation_limit";
pub const RPC_ERROR_MISSING_SESSION_ID:&str="missing_session_id";
pub const RPC_ERROR_MULTI_SESSION_DISABLED:&str="multi_session_disabled";
pub const RPC_ERROR_INVALID_PATH:&str="invalid_path";
pub const RPC_ERROR_OPEN_FAILED:&str="open_failed";
pub const RPC_ERROR_MEDIA_NOT_FOUND:&str="media_not_found";
pub const RPC_ERROR_INVALID_SESSION_CONTEXT:&str="invalid_session_context";
pub const RPC_ERROR_INVALID_SESSION_KIND:&str="invalid_session_kind";
pub const RPC_ERROR_INVALID_LAUNCH_PROFILE:&str="invalid_launch_profile";
pub const RPC_ERROR_INVALID_SESSION_ID:&str="invalid_session_id";
pub const RPC_ERROR_SESSION_ID_IN_USE:&str="session_id_in_use";
pub const RPC_ERROR_HOST_MEMORY_PRESSURE:&str="host_memory_pressure";
pub const RPC_ERROR_STREAMING:&str="streaming";
pub const RPC_ERROR_ENTRY_NOT_FOUND:&str="not_found";
pub const RPC_ERROR_NOT_ASSISTANT:&str="not_assistant";
pub const RPC_ERROR_NOT_USER:&str="not_user";
pub const RPC_ERROR_EMPTY_TEXT:&str="empty";
pub const RPC_ERROR_STALE_LEAF:&str="stale_leaf";

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RpcCommand {
    #[serde(skip_serializing_if="Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub session_id: Option<String>,
    #[serde(flatten)]
    pub body: RpcCommandBody,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="snake_case",rename_all_fields="camelCase")]
pub enum RpcCommandBody {
    Prompt { message:String, #[serde(skip_serializing_if="Option::is_none")] images:Option<Vec<Value>>, #[serde(skip_serializing_if="Option::is_none")] streaming_behavior:Option<StreamingBehavior>, #[serde(skip_serializing_if="Option::is_none")] thinking_level:Option<String>, #[serde(skip_serializing_if="Option::is_none")] session_title_prompt:Option<SessionTitlePrompt>, #[serde(skip_serializing_if="Option::is_none")] expand_prompt_templates:Option<bool> },
    SendCustomMessage { custom_type:String, content:Value, display:bool, #[serde(skip_serializing_if="Option::is_none")] details:Option<Value>, #[serde(skip_serializing_if="Option::is_none")] trigger_turn:Option<bool>, #[serde(skip_serializing_if="Option::is_none")] deliver_as:Option<DeliveryMode> },
    AppendUserMessage { content:Value },
    AppendSessionEntry { entry:Value },
    Steer { message:String, #[serde(skip_serializing_if="Option::is_none")] images:Option<Vec<Value>>, #[serde(skip_serializing_if="Option::is_none")] enqueue_order:Option<f64> },
    FollowUp { message:String, #[serde(skip_serializing_if="Option::is_none")] images:Option<Vec<Value>>, #[serde(skip_serializing_if="Option::is_none")] enqueue_order:Option<f64> },
    Abort, AbortCompaction, Reload, CheckReloadVeto,
    ClearQueue { #[serde(skip_serializing_if="Option::is_none")] abort_will_follow:Option<bool> },
    GetSteeringMessages, GetFollowUpMessages, AbortBranchSummary,
    NewSession { #[serde(skip_serializing_if="Option::is_none")] parent_session:Option<String> },
    GetState,
    SetModel { provider:String, model_id:String },
    SetFavoriteModels { models:Vec<Value> },
    SetScopedModels { models:Vec<Value> },
    CycleModel { #[serde(skip_serializing_if="Option::is_none")] direction:Option<ModelDirection> },
    GetAvailableModels,
    SetThinkingLevel { level:String, #[serde(skip_serializing_if="Option::is_none")] scope:Option<ThinkingScope> },
    CycleThinkingLevel, GetAvailableThinkingLevels,
    SetFastMode { enabled:bool }, GetFastMode,
    SetSteeringMode { mode:QueueMode }, SetFollowUpMode { mode:QueueMode },
    Compact { #[serde(skip_serializing_if="Option::is_none")] custom_instructions:Option<String> },
    SetAutoCompaction { enabled:bool }, SetAutoRetry { enabled:bool }, AbortRetry,
    Bash { command:String, #[serde(skip_serializing_if="Option::is_none")] bash_id:Option<String>, #[serde(skip_serializing_if="Option::is_none")] exclude_from_context:Option<bool>, #[serde(skip_serializing_if="Option::is_none")] execution_id:Option<String>, #[serde(skip_serializing_if="Option::is_none")] operations:Option<BTreeMap<String,Value>> },
    RecordBashResult { command:String, result:Value, #[serde(skip_serializing_if="Option::is_none")] exclude_from_context:Option<bool> },
    AbortBash, CleanupBashOutput { path:String },
    SetLabel { entry_id:String, #[serde(skip_serializing_if="Option::is_none")] label:Option<String> },
    NavigateTree { #[serde(skip_serializing_if="Option::is_none")] entry_id:Option<String>, #[serde(skip_serializing_if="Option::is_none")] target_id:Option<String>, #[serde(skip_serializing_if="Option::is_none")] intent:Option<NavigationIntent>, #[serde(skip_serializing_if="Option::is_none")] expected_leaf_id:Option<String>, #[serde(skip_serializing_if="Option::is_none")] summarize:Option<bool>, #[serde(skip_serializing_if="Option::is_none")] custom_instructions:Option<String>, #[serde(skip_serializing_if="Option::is_none")] replace_instructions:Option<bool>, #[serde(skip_serializing_if="Option::is_none")] label:Option<String> },
    GetSessionStats,
    ExportHtml { #[serde(skip_serializing_if="Option::is_none")] output_path:Option<String>, #[serde(skip_serializing_if="Option::is_none")] theme_name:Option<String> },
    ExportJsonl { #[serde(skip_serializing_if="Option::is_none")] output_path:Option<String> },
    SwitchSession { session_path:String, #[serde(skip_serializing_if="Option::is_none")] cwd_override:Option<String> },
    Fork { entry_id:String, #[serde(skip_serializing_if="Option::is_none")] position:Option<ForkPosition> },
    EditAssistantMessage { entry_id:String, text:String, #[serde(skip_serializing_if="Option::is_none")] expected_leaf_id:Option<String>, #[serde(skip_serializing_if="Option::is_none")] summarize:Option<bool>, #[serde(skip_serializing_if="Option::is_none")] custom_instructions:Option<String> },
    EditUserMessage { entry_id:String, text:String, #[serde(skip_serializing_if="Option::is_none")] expected_leaf_id:Option<String>, #[serde(skip_serializing_if="Option::is_none")] summarize:Option<bool>, #[serde(skip_serializing_if="Option::is_none")] custom_instructions:Option<String> },
    Clone, GetForkMessages,
    GetEntries { #[serde(skip_serializing_if="Option::is_none")] since:Option<String> },
    GetTree, GetLastAssistantText,
    SetSessionName { name:String },
    ImportJsonl { input_path:String, #[serde(skip_serializing_if="Option::is_none")] cwd_override:Option<String> },
    GetMessages, GetMedia { tool_call_id:String, content_index:f64 },
    GetCommands, GetLoadedSurfaces,
    ExtensionRequest { name:String, #[serde(skip_serializing_if="Option::is_none")] data:Option<Value> },
    GetAuthProviders,
    LoginStart { provider:String }, LoginCancel { provider:String }, LoginApiKey { provider:String, key:String }, Logout { provider:String },
    GetProviderAccounts { provider:String }, AccountPin { provider:String, name:Option<String> }, AccountRemove { provider:String, name:String },
    SetClientInfo { width:f64, #[serde(skip_serializing_if="Option::is_none")] capabilities:Option<Vec<String>> },
    GetProtocolInfo,
    OpenSession { #[serde(skip_serializing_if="Option::is_none")] session_path:Option<String>, #[serde(skip_serializing_if="Option::is_none")] cwd:Option<String>, #[serde(skip_serializing_if="Option::is_none")] provider:Option<String>, #[serde(skip_serializing_if="Option::is_none")] model_id:Option<String>, #[serde(skip_serializing_if="Option::is_none")] thinking_level:Option<String>, #[serde(skip_serializing_if="Option::is_none")] permission_preset:Option<String>, #[serde(rename="retain_on_disconnect",skip_serializing_if="Option::is_none")] retain_on_disconnect:Option<bool>, #[serde(skip_serializing_if="Option::is_none")] kind:Option<SessionKind>, #[serde(skip_serializing_if="Option::is_none")] context:Option<BTreeMap<String,String>>, #[serde(rename="auto_title",skip_serializing_if="Option::is_none")] auto_title:Option<bool>, #[serde(skip_serializing_if="Option::is_none")] durable_session_id:Option<String> },
    CloseSession,
    ListSessions { #[serde(rename="include_workers",skip_serializing_if="Option::is_none")] include_workers:Option<bool> },
}

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum StreamingBehavior { Steer, FollowUp }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum DeliveryMode { Steer, FollowUp, NextTurn }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="kebab-case")]
pub enum QueueMode { All, OneAtATime }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ModelDirection { Forward, Backward }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ThinkingScope { Turn }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum NavigationIntent { Select, Resume }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ForkPosition { Before, At }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum SessionKind { Interactive, Worker }
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum SessionTitlePrompt { Text(String), Enabled(bool) }

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RpcQuestionSpec {
    pub id:String,
    pub header:String,
    pub question:String,
    pub options:Vec<RpcQuestionOption>,
    pub multi_select:bool,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct RpcQuestionOption {
    pub label:String,
    #[serde(skip_serializing_if="Option::is_none")]
    pub description:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct RpcQuestionAnswer {
    pub selected:Vec<String>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub text:Option<String>,
}
pub type RpcQuestionAnswers = BTreeMap<String,RpcQuestionAnswer>;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum RpcQuestionOutcome {
    #[serde(rename="answered")] Answered,
    #[serde(rename="comment-submitted")] CommentSubmitted,
    #[serde(rename="timed_out")] TimedOut,
    #[serde(rename="cancelled")] Cancelled,
    #[serde(rename="orphaned-after-restart")] OrphanedAfterRestart,
    #[serde(rename="unavailable")] Unavailable,
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct RpcExtensionUiRequest {
    #[serde(rename="type")] pub record_type:ExtensionUiRequestType,
    pub id:String,
    #[serde(flatten)] pub body:RpcExtensionUiRequestBody,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum ExtensionUiRequestType { #[serde(rename="extension_ui_request")] ExtensionUiRequest }
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="method",rename_all_fields="camelCase")]
pub enum RpcExtensionUiRequestBody {
    #[serde(rename="select")] Select { title:String,options:Vec<String>,#[serde(skip_serializing_if="Option::is_none")] timeout:Option<f64> },
    #[serde(rename="confirm")] Confirm { title:String,message:String,#[serde(skip_serializing_if="Option::is_none")] timeout:Option<f64> },
    #[serde(rename="input")] Input { title:String,#[serde(skip_serializing_if="Option::is_none")] placeholder:Option<String>,#[serde(skip_serializing_if="Option::is_none")] timeout:Option<f64> },
    #[serde(rename="editor")] Editor { title:String,#[serde(skip_serializing_if="Option::is_none")] prefill:Option<String> },
    #[serde(rename="notify")] Notify { message:String,#[serde(skip_serializing_if="Option::is_none")] notify_type:Option<NotifyType> },
    #[serde(rename="setStatus")] SetStatus { status_key:String,#[serde(skip_serializing_if="Option::is_none")] status_text:Option<String> },
    #[serde(rename="setWidget")] SetWidget { widget_key:String,#[serde(skip_serializing_if="Option::is_none")] widget_lines:Option<Vec<String>>,#[serde(skip_serializing_if="Option::is_none")] widget_placement:Option<WidgetPlacement> },
    #[serde(rename="setHeader")] SetHeader { #[serde(skip_serializing_if="Option::is_none")] widget_lines:Option<Vec<String>> },
    #[serde(rename="setFooter")] SetFooter { #[serde(skip_serializing_if="Option::is_none")] widget_lines:Option<Vec<String>> },
    #[serde(rename="setTitle")] SetTitle { title:String },
    #[serde(rename="set_editor_text")] SetEditorText { text:String },
    #[serde(rename="custom_unsupported")] CustomUnsupported { extension_name:String },
    #[serde(rename="question")] Question { request_id:String,tool_call_id:String,wait_for_answer:bool,questions:Vec<RpcQuestionSpec>,timeout:f64,asked_at_ms:f64,deadline_at_ms:f64,remaining_ms:f64 },
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum NotifyType { Info, Warning, Error }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum WidgetPlacement { AboveEditor, BelowEditor }
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct RpcExtensionUiResponse {
    #[serde(rename="type")] pub record_type:ExtensionUiResponseType,
    pub id:String,
    #[serde(flatten)] pub body:RpcExtensionUiResponseBody,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum ExtensionUiResponseType { #[serde(rename="extension_ui_response")] ExtensionUiResponse }
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum RpcExtensionUiResponseBody {
    Value { value:String },
    Confirmed { confirmed:bool },
    Cancelled { cancelled:bool },
    Answers { answers:RpcQuestionAnswers,#[serde(skip_serializing_if="Option::is_none")] comment:Option<String> },
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RpcExtensionUiProgress {
    #[serde(rename="type")] pub record_type:ExtensionUiProgressType,
    pub id:String,
    #[serde(skip_serializing_if="Option::is_none")] pub answers:Option<RpcQuestionAnswers>,
    #[serde(skip_serializing_if="Option::is_none")] pub comment:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub session_id:Option<String>,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum ExtensionUiProgressType { #[serde(rename="extension_ui_progress")] ExtensionUiProgress }
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum RpcInboundRecord { Command(RpcCommand),UiResponse(RpcExtensionUiResponse),UiProgress(RpcExtensionUiProgress) }

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="snake_case",rename_all_fields="camelCase")]
pub enum RpcHostLifecycleEvent {
    HostSuperseded { instance_id:String,generation:f64,successor:Option<RpcHostSuccessor> },
    HostStalled { drift_ms:f64,#[serde(skip_serializing_if="Option::is_none")]session_id:Option<String>,#[serde(skip_serializing_if="Option::is_none")]tool:Option<String> },
    HostMemoryPressure { rss_mb:f64,sessions:f64 },
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct RpcHostSuccessor { pub socket:String }

#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="snake_case",rename_all_fields="camelCase")]
pub enum RpcSessionLifecycleEvent {
    SessionReplaced { durable_session_id:String,#[serde(skip_serializing_if="Option::is_none")]session_file:Option<String>,cwd:String,#[serde(skip_serializing_if="Option::is_none")]session_name:Option<String> },
    SessionParked { session_id:String,session_path:String },
    SessionClosed { session_id:String,#[serde(skip_serializing_if="Option::is_none")]reason:Option<String>,#[serde(skip_serializing_if="Option::is_none")]session_path:Option<String> },
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="snake_case",rename_all_fields="camelCase")]
pub enum RpcQuestionEvent {
    QuestionUpdated { id:String,deadline_at_ms:f64,remaining_ms:f64 },
    QuestionResolved { id:String,request_id:String,tool_call_id:String,outcome:RpcQuestionOutcome,answers:RpcQuestionAnswers,#[serde(skip_serializing_if="Option::is_none")]comment:Option<String>,unanswered:Vec<String>,#[serde(skip_serializing_if="Option::is_none")]deadline_at_ms:Option<f64> },
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct RpcOpenQueuedEvent {
    #[serde(rename="type")]pub record_type:OpenQueuedType,
    pub for_request:String,pub position:f64,pub in_flight:f64,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum OpenQueuedType { #[serde(rename="queued")] Queued }

#[derive(Clone,Debug,PartialEq,Serialize)]
#[serde(rename_all="camelCase")]
pub struct RpcResponse {
    #[serde(skip_serializing_if="Option::is_none")]
    pub id:Option<String>,
    #[serde(rename="type")]
    pub record_type:ResponseRecordType,
    pub command:String,
    #[serde(skip_serializing_if="Option::is_none")]
    pub session_id:Option<String>,
    #[serde(flatten)]
    pub result:RpcResponseResult,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ResponseRecordType { Response }
#[derive(Clone,Debug,PartialEq)]
pub enum RpcResponseResult {
    Success { data:Option<Value> },
    Error { error:String, error_code:Option<String>, error_data:Option<Value> },
}
impl Serialize for RpcResponseResult {
    fn serialize<S:serde::Serializer>(&self, serializer:S) -> Result<S::Ok,S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::Success { data } => {
                map.serialize_entry("success",&true)?;
                if let Some(data) = data { map.serialize_entry("data",data)?; }
            }
            Self::Error { error,error_code,error_data } => {
                map.serialize_entry("success",&false)?;
                map.serialize_entry("error",error)?;
                if let Some(code) = error_code { map.serialize_entry("errorCode",code)?; }
                if let Some(data) = error_data { map.serialize_entry("errorData",data)?; }
            }
        }
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test] fn prompt_roundtrip_preserves_correlation_and_routing() { let value = json!({"id":"r1","sessionId":"route","type":"prompt","message":"hi","streamingBehavior":"followUp","sessionTitlePrompt":false}); let command:RpcCommand = serde_json::from_value(value.clone()).unwrap(); assert_eq!(serde_json::to_value(command).unwrap(),value); }
    #[test] fn open_preserves_mixed_case_wire_names() { let value = json!({"id":"r1","type":"open_session","retain_on_disconnect":true,"auto_title":false,"durableSessionId":"durable","kind":"worker","context":{"label":"test"}}); let command:RpcCommand = serde_json::from_value(value.clone()).unwrap(); assert_eq!(serde_json::to_value(command).unwrap(),value); }
    #[test] fn queue_mode_uses_hyphenated_wire_value() { let command:RpcCommand = serde_json::from_value(json!({"type":"set_steering_mode","mode":"one-at-a-time"})).unwrap(); assert!(matches!(command.body,RpcCommandBody::SetSteeringMode { mode:QueueMode::OneAtATime })); }
    #[test] fn close_uses_envelope_routing_handle() { let command:RpcCommand = serde_json::from_value(json!({"type":"close_session","sessionId":"route"})).unwrap(); assert_eq!(command.session_id.as_deref(),Some("route")); assert!(matches!(command.body,RpcCommandBody::CloseSession)); }
    #[test] fn list_workers_remains_snake_case() { let value = json!({"type":"list_sessions","include_workers":true}); let command:RpcCommand = serde_json::from_value(value.clone()).unwrap(); assert_eq!(serde_json::to_value(command).unwrap(),value); }
    #[test] fn response_success_is_boolean_and_absent_data_is_omitted() { let response = RpcResponse { id:Some("r1".into()),record_type:ResponseRecordType::Response,command:"abort".into(),session_id:None,result:RpcResponseResult::Success { data:None } }; assert_eq!(serde_json::to_value(response).unwrap(),json!({"id":"r1","type":"response","command":"abort","success":true})); }
    #[test] fn response_failure_preserves_error_code_and_data() { let response = RpcResponse { id:None,record_type:ResponseRecordType::Response,command:"prompt".into(),session_id:Some("route".into()),result:RpcResponseResult::Error { error:"streaming".into(),error_code:Some("streaming".into()),error_data:Some(json!({"retry":false})) } }; assert_eq!(serde_json::to_value(response).unwrap(),json!({"type":"response","command":"prompt","sessionId":"route","success":false,"error":"streaming","errorCode":"streaming","errorData":{"retry":false}})); }
    #[test] fn question_response_and_progress_are_inbound_records() { for value in [json!({"type":"extension_ui_response","id":"q","answers":{"one":{"selected":["yes"],"text":"extra"}},"comment":"ok"}),json!({"type":"extension_ui_progress","id":"q","sessionId":"route","comment":"draft"})] { let record:RpcInboundRecord = serde_json::from_value(value.clone()).unwrap(); assert_eq!(serde_json::to_value(record).unwrap(),value); } }
    #[test] fn widget_wire_names_preserve_mixed_conventions() { let value = json!({"type":"extension_ui_request","id":"w","method":"setWidget","widgetKey":"status","widgetLines":["ready"],"widgetPlacement":"belowEditor"}); let request:RpcExtensionUiRequest = serde_json::from_value(value.clone()).unwrap(); assert_eq!(serde_json::to_value(request).unwrap(),value); }
    #[test] fn cleared_widget_omits_undefined_lines() { let request = RpcExtensionUiRequest { record_type:ExtensionUiRequestType::ExtensionUiRequest,id:"w".into(),body:RpcExtensionUiRequestBody::SetWidget { widget_key:"status".into(),widget_lines:None,widget_placement:None } }; assert_eq!(serde_json::to_value(request).unwrap(),json!({"type":"extension_ui_request","id":"w","method":"setWidget","widgetKey":"status"})); }
}
