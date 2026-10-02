use maho_ext_compaction::openai_remote_schema::*;
use serde_json::{Value,json};
fn details()->Value { json!({"schema":OPENAI_REMOTE_COMPACTION_SCHEMA,"mode":"openai-remote","provider":"openai","api":"openai-responses","modelId":"m","responseId":"r","createdAt":1,"requestInputItemCount":2,"retainedInputItemCount":1,"replacementInput":[{"type":"compaction","encrypted_content":"opaque"},null,1]}) }
#[test] fn legacy_details_parse_with_default_transport_and_filtered_items() {
    let parsed=get_openai_remote_compaction_details(&details()).expect("details");
    assert_eq!(parsed.transport,OpenAiRemoteTransport::CompactEndpoint);assert_eq!(parsed.replacement_input.len(),1);assert!(parsed.origin.is_none());
}
#[test] fn checkpoint_fields_are_required() {
    for key in ["schema","mode","provider","api","modelId","responseId","createdAt","requestInputItemCount","retainedInputItemCount","replacementInput"] {
        let mut value=details();value.as_object_mut().expect("object").remove(key);assert!(get_openai_remote_compaction_details(&value).is_none(),"{key}");
    }
}
#[test] fn foreign_responses_identity_is_valid_but_foreign_codex_is_not() {
    let mut value=details();value["provider"]=json!("foreign");assert!(get_openai_remote_compaction_details(&value).is_some());value["api"]=json!("openai-codex-responses");assert!(get_openai_remote_compaction_details(&value).is_none());value["provider"]=json!("chatgpt-subscription");assert!(get_openai_remote_compaction_details(&value).is_some());
}
#[test] fn origins_require_a_fingerprint_prefix() {
    let mut value=details();value["origin"]=json!({"endpoint":"https://api.openai.com/v1","trustDomain":"https://api.openai.com","authTenantFingerprint":"sha256:abc"});assert!(get_openai_remote_compaction_details(&value).expect("details").origin.is_some());value["origin"]["authTenantFingerprint"]=json!("abc");assert!(get_openai_remote_compaction_details(&value).expect("details").origin.is_none());
}
#[test] fn compaction_trigger_is_not_a_retained_checkpoint() {
    assert!(!is_openai_remote_compaction_output_item(&json!({"type":"context_compaction"})));
    for kind in ["compaction","context_compaction"] {assert!(is_openai_remote_compaction_output_item(&json!({"type":kind,"encrypted_content":"opaque"})));}
}
#[test] fn retained_output_excludes_assistant_and_functions() {
    for role in ["user","system","developer"] {assert!(is_retained_remote_output_item(&json!({"type":"message","role":role})));}
    for role in ["assistant","tool"] {assert!(!is_retained_remote_output_item(&json!({"type":"message","role":role})));}
    assert!(!is_retained_remote_output_item(&json!({"type":"function_call"})));
}
#[test] fn streamed_input_accepts_user_with_or_without_type() {
    for value in [json!({"type":"message","role":"user"}),json!({"role":"user"})] {assert!(is_retained_responses_stream_input_item(&value));}
    assert!(!is_retained_responses_stream_input_item(&json!({"type":"message","role":"system"})));
}
