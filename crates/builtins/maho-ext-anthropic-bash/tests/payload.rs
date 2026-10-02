use maho_ext_anthropic_bash::{parse_enabled, add_anthropic_bash_to_payload};
use serde_json::json;

#[test]
fn disabled_when_unset() {
    assert!(!parse_enabled(None));
}

#[test]
fn disabled_when_false_values() {
    for value in ["0","false","no","off",""] { assert!(!parse_enabled(Some(value))); }
}

#[test]
fn unchanged_when_openai_responses() {
    let p=json!({"tools":[{"name":"bash"}]}); assert_eq!(add_anthropic_bash_to_payload(Some("openai-responses"),&p,true),p);
}

#[test]
fn unchanged_when_openai_completions() {
    let p=json!({"tools":[]}); assert_eq!(add_anthropic_bash_to_payload(Some("openai-completions"),&p,true),p);
}

#[test]
fn unchanged_when_google() {
    let p=json!({"tools":[]}); assert_eq!(add_anthropic_bash_to_payload(Some("google-generative-ai"),&p,true),p);
}

#[test]
fn injects_when_no_native() {
    assert_eq!(add_anthropic_bash_to_payload(Some("anthropic-messages"),&json!({}),true)["tools"],json!([{ "type":"bash_20250124","name":"bash" }]));
}

#[test]
fn replaces_when_function_bash() {
    let p=json!({"tools":[{"name":"bash","description":"function"},{"name":"read"}]}); let r=add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,true); assert_eq!(r["tools"],json!([{ "name":"read" },{"type":"bash_20250124","name":"bash"}]));
}

#[test]
fn preserves_when_native() {
    let p=json!({"tools":[{"type":"bash_20250124","name":"bash"}]}); assert_eq!(add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,true),p);
}

#[test]
fn preserves_when_other_version() {
    let p=json!({"tools":[{"type":"bash_20251215","name":"bash"}]}); assert_eq!(add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,true),p);
}

#[test]
fn preserves_function_when_disabled() {
    let p=json!({"tools":[{"name":"bash"}]}); assert_eq!(add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,false),p);
}

#[test]
fn preserves_function_when_api_mismatch() {
    let p=json!({"tools":[{"name":"bash"}]}); assert_eq!(add_anthropic_bash_to_payload(None,&p,true),p);
}

#[test]
fn preserves_when_capitalized() {
    let p=json!({"tools":[{"name":"Bash"}]}); assert_eq!(add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,true)["tools"][0],p["tools"][0]);
}

#[test]
fn preserves_when_different_name() {
    let p=json!({"tools":[{"name":"shell"}]}); assert_eq!(add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,true)["tools"][0],p["tools"][0]);
}

#[test]
fn preserves_when_other_tools() {
    let p=json!({"tools":[{"name":"read","input_schema":{"type":"object"}}]}); assert_eq!(add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,true)["tools"][0],p["tools"][0]);
}

#[test]
fn deduplicates_when_function_and_native() {
    let p=json!({"tools":[{"name":"bash"},{"type":"bash_20250124","name":"bash"}]}); assert_eq!(add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,true)["tools"],json!([{ "type":"bash_20250124","name":"bash" }]));
}

#[test]
fn false_when_missing_environment() {
    assert!(!parse_enabled(None));
}

#[test]
fn true_when_truthy_values() {
    for value in ["1","true","yes","on"," TRUE ","\tYes\n"] { assert!(parse_enabled(Some(value))); }
}

#[test]
fn false_when_unknown_values() {
    for value in ["0","false","no","off","","garbage","2","enable"] { assert!(!parse_enabled(Some(value))); }
}

#[test]
fn preserves_input_when_injected() {
    let p=json!({"tools":[{"name":"bash"}]}); let before=p.clone(); let r=add_anthropic_bash_to_payload(Some("anthropic-messages"),&p,true); assert_ne!(r,p); assert_eq!(p,before);
}

