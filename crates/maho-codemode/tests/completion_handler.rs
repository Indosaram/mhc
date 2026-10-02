use maho_codemode::completion::handler::*;
use serde_json::json;

#[test] fn opts_override_only_typed_strings_and_present_schema() {
    let request = normalize_request(CompletionRequest { prompt:"p".into(),model:Some("default".into()),system:Some("base".into()),schema:None,opts:Some(json!({"model":"slow","system":false,"schema":null})) });
    assert_eq!(request.model.as_deref(),Some("slow")); assert_eq!(request.system.as_deref(),Some("base")); assert_eq!(request.schema,Some(json!(null)));
}
#[test] fn tier_validation() {
    assert_eq!(resolve_completion_tier(None).unwrap(),CompletionTier::Default);
    assert_eq!(resolve_completion_tier(Some("smol")).unwrap(),CompletionTier::Smol);
    assert_eq!(resolve_completion_tier(Some("slow")).unwrap(),CompletionTier::Slow);
    assert!(resolve_completion_tier(Some("provider/model")).is_err());
}

fn message(text: &str) -> maho_ai::types::AssistantMessage {
    use maho_ai::types::*;
    AssistantMessage { content:vec![ContentBlock::text(text)],api:"fake-api".into(),provider:"fake".into(),model:"test".into(),response_model:None,response_id:None,provider_thinking_level:None,diagnostics:None,usage:Usage::default(),stop_reason:StopReason::Stop,stop_details:None,deferred:None,error_message:None,abort_source:None,raw_stop_reason:None,end_turn:None,timestamp:0 }
}

#[test]
fn plain_completion_joins_text_and_reports_selected_model() {
    let mut response = message("first");
    response.content.push(maho_ai::types::ContentBlock::text("second"));
    assert_eq!(format_completion(&response,"provider","chosen",false).unwrap(),json!({"text":"first\nsecond","details":{"model":"provider/chosen","structured":false}}));
}

#[test]
fn structured_json_and_parse_error_are_values() {
    assert_eq!(format_completion(&message("null"),"p","m",true).unwrap()["value"],json!(null));
    assert!(format_completion(&message("not json"),"p","m",true).unwrap()["value"]["parseError"].is_string());
}

#[test]
fn error_abort_and_empty_output_fail() {
    let mut response=message("ignored");
    response.stop_reason=maho_ai::types::StopReason::Error;
    response.error_message=Some("provider rejected".into());
    assert_eq!(format_completion(&response,"p","m",false).unwrap_err().0,"provider rejected");
    response.stop_reason=maho_ai::types::StopReason::Aborted;
    assert!(format_completion(&response,"p","m",false).is_err());
    assert!(format_completion(&message(""),"p","m",false).is_err());
}
