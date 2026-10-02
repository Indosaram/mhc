use maho_core::compaction::compaction::estimate_tokens;
use serde_json::json;
pub const TOOL_ADMISSION_MARKER_PREFIX:&str="[tool result projected:";
#[derive(Debug,PartialEq,Eq)]
pub struct AdmitToolResultOutput {pub text:String,pub projected:bool}
pub fn resolve_tool_result_admission_cap_tokens(context_window:u64)->u64 { (context_window/20).clamp(8192,50000) }
fn estimate_text_tokens(text:&str)->u64 {estimate_tokens(&json!({"role":"user","content":text,"timestamp":0}))}
fn build_excerpt(units:&[u16],budget_chars:usize,total_tokens:u64)->String {
    let head_chars=budget_chars*3/5;
    let tail_chars=budget_chars/5;
    let head=String::from_utf16_lossy(&units[..head_chars.min(units.len())]);
    let tail=String::from_utf16_lossy(&units[head_chars.max(units.len().saturating_sub(tail_chars)).min(units.len())..]);
    let kept=estimate_text_tokens(&head)+estimate_text_tokens(&tail);
    format!("{head}\n{TOOL_ADMISSION_MARKER_PREFIX} kept {kept} of ~{total_tokens} tokens]\n{tail}")
}
pub fn admit_tool_result_within_budget(text:&str,budget_tokens:u64)->AdmitToolResultOutput {
    let total=estimate_text_tokens(text);
    if total<=budget_tokens {return AdmitToolResultOutput {text:text.to_owned(),projected:false};}
    if budget_tokens==0 {return AdmitToolResultOutput {text:String::new(),projected:true};}
    let units:Vec<_>=text.encode_utf16().collect();
    let budget=usize::try_from(budget_tokens).unwrap_or(usize::MAX);
    let denominator=usize::try_from(total.max(1)).unwrap_or(usize::MAX);
    let mut budget_chars=budget.saturating_mul(units.len())/denominator;
    let mut excerpt=build_excerpt(&units,budget_chars,total);
    while estimate_text_tokens(&excerpt)>budget_tokens && budget_chars>0 {
        budget_chars=budget_chars*4/5;
        excerpt=build_excerpt(&units,budget_chars,total);
    }
    AdmitToolResultOutput {text:if estimate_text_tokens(&excerpt)<=budget_tokens {excerpt} else {String::new()},projected:true}
}
pub fn admit_tool_result(text:&str,context_window:u64)->AdmitToolResultOutput {
    admit_tool_result_within_budget(text,resolve_tool_result_admission_cap_tokens(context_window))
}
