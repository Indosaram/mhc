use std::sync::LazyLock;
use regex::Regex;
use serde_json::Value;
use crate::types::ApplyPatchWireMode;
static GPT_ID:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?i)(?:^|[/@:._-])gpt(?:[._-]|[0-9])").expect("literal pattern"));
pub fn get_apply_patch_wire_mode(model:Option<(&str,&str)>)->ApplyPatchWireMode {
    let Some((api,id))=model else { return ApplyPatchWireMode::None; }; if !GPT_ID.is_match(id) { return ApplyPatchWireMode::None; }
    match api { "openai-responses"|"azure-openai-responses"|"openai-codex-responses"=>ApplyPatchWireMode::Freeform,"openai-completions"=>ApplyPatchWireMode::Json,_=>ApplyPatchWireMode::None }
}
pub fn is_openai_gpt_model(model:Option<(&str,&str)>)->bool { get_apply_patch_wire_mode(model)==ApplyPatchWireMode::Freeform }
pub fn without_apply_patch(names:&[String])->Vec<String> { names.iter().filter(|name|name.as_str()!="apply_patch").cloned().collect() }
pub fn replace_edit_tools_with_apply_patch(names:&[String])->Vec<String> {
    let insert=names.iter().position(|name|matches!(name.as_str(),"write"|"edit"|"apply_patch"));
    let mut filtered:Vec<_>=names.iter().filter(|name|!matches!(name.as_str(),"write"|"edit"|"apply_patch")).cloned().collect();
    if let Some(index)=insert { filtered.insert(index.min(filtered.len()),"apply_patch".into()); } filtered
}
pub fn has_apply_patch_failures(details:&Value)->bool { details.get("result").and_then(|result|result.get("failures")).and_then(Value::as_array).is_some_and(|failures|!failures.is_empty()) }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn gateway_gpt_ids_use_api_gate() { for id in ["codex/gpt-6-astra","global.openai.gpt-6-astra","gateway:GPT_6_ASTRA","gpt5"] { assert_eq!(get_apply_patch_wire_mode(Some(("openai-responses",id))),ApplyPatchWireMode::Freeform); assert_eq!(get_apply_patch_wire_mode(Some(("openai-completions",id))),ApplyPatchWireMode::Json); } }
    #[test] fn false_gpt_substrings_are_rejected() { for id in ["xgpt-5.6-proxy","deepseek-v3-gptq","gpt"] { assert_eq!(get_apply_patch_wire_mode(Some(("openai-responses",id))),ApplyPatchWireMode::None); } assert!(!is_openai_gpt_model(Some(("anthropic-messages","gpt-5")))); }
    #[test] fn replacement_preserves_first_edit_position() { let names=["read","write","bash","edit","apply_patch"].map(String::from); assert_eq!(replace_edit_tools_with_apply_patch(&names),["read","apply_patch","bash"]); assert_eq!(replace_edit_tools_with_apply_patch(&["read".into()]),["read"]); }
    #[test] fn malformed_failures_do_not_claim_partial_application() { assert!(!has_apply_patch_failures(&serde_json::json!({"result":{"failures":true}}))); assert!(has_apply_patch_failures(&serde_json::json!({"result":{"failures":[{}]}}))); }
}
