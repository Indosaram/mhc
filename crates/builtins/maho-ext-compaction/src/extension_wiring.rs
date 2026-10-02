use serde_json::{Value,json};
use crate::checkpoint_state::get_latest_checkpoint;
pub fn estimate_pending_prompt_tokens(prompt:Option<&str>,image_count:usize)->usize {prompt.unwrap_or_default().encode_utf16().count().div_ceil(4)+image_count*1200}
pub fn get_prompt_context_window(window:f64,max_tokens:Option<f64>)->f64 {
    match max_tokens {Some(max) if max.is_finite() && max>0.0 && window>0.0 => window-max.min((window*0.5).floor()),_=>window}
}
pub fn with_additional_tokens(usage:&Value,additional:f64)->Value {
    let Some(tokens)=usage.get("tokens").and_then(Value::as_f64) else {return usage.clone();};
    if additional<=0.0 {return usage.clone();}
    let mut result=usage.clone();result["tokens"]=json!(tokens+additional);
    if let Some(window)=usage.get("contextWindow").and_then(Value::as_f64).filter(|w|*w>0.0) {result["percent"]=json!((tokens+additional)/window*100.0);}
    result
}
pub fn is_monitorable_message(message:&Value)->bool {message.get("content").is_some_and(Value::is_array)}
pub fn is_aborted_assistant_message(message:&Value)->bool {message.get("role").and_then(Value::as_str)==Some("assistant") && message.get("stopReason").and_then(Value::as_str)==Some("aborted")}
pub fn is_required_compaction_fallback_reason(reason:&str)->bool {matches!(reason,"manual"|"threshold"|"overflow")}
pub fn recent_checkpoint(entries:&[Value],now:f64)->Option<Value> {
    let checkpoint=get_latest_checkpoint(entries)?;
    let timestamp=checkpoint.get("timestamp").and_then(Value::as_f64)?;
    if timestamp!=0.0 && now-timestamp<=60000.0 {Some(checkpoint)} else {None}
}
pub fn compaction_feedback(applied:bool,reason:&str,aborted:bool,remote_fallback:Option<&str>)->Option<Value> {
    if applied || reason=="rejected" {return None;}
    let parts:Vec<_>=remote_fallback.into_iter().chain(std::iter::once(reason)).filter(|p|!p.is_empty()).collect();
    let mut result=json!({"reason":"extension","aborted":aborted});
    if !aborted && !parts.is_empty() {result["errorMessage"]=json!(format!("Compaction did not apply: {}",parts.join("; local fallback ")));}
    Some(result)
}

pub fn end_compaction_feedback(
    context: &maho_ext_api::ExtensionContext,
    signal: Option<maho_ext_api::AbortSignal>,
    applied: bool,
    reason: &str,
    remote_fallback: Option<&str>,
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let aborted = signal.as_ref().is_some_and(maho_ext_api::AbortSignal::is_aborted);
    let Some(feedback) = compaction_feedback(applied, reason, aborted, remote_fallback) else { return Ok(()); };
    context.end_compaction(maho_ext_api::EndCompactionOptions {
        reason: maho_ext_api::CompactionReason::Extension,
        signal,
        aborted: Some(aborted),
        error_message: feedback.get("errorMessage").and_then(Value::as_str).map(str::to_owned),
    })
}

pub fn create_blocking_remote_compaction_event(
    context: &maho_ext_api::ExtensionContext,
    preparation: maho_ext_api::CompactionPreparation,
    request_id: String,
    custom_instructions: String,
    signal: maho_ext_api::AbortSignal,
) -> maho_ext_api::SessionBeforeCompactEvent {
    maho_ext_api::SessionBeforeCompactEvent {
        reason: maho_ext_api::CompactionReason::Extension,
        will_retry: false,
        request_id,
        preparation,
        branch_entries: context.session_manager.get_branch(),
        custom_instructions: Some(custom_instructions),
        signal,
    }
}

pub struct AbortLink {
    source: maho_ai::utils::abort::AbortSignal,
    listener: maho_ai::utils::abort::ListenerId,
}
impl Drop for AbortLink {
    fn drop(&mut self) { self.source.remove_abort_listener(self.listener); }
}
pub fn link_abort_signal(source: Option<&maho_ai::utils::abort::AbortSignal>, target: &maho_ai::utils::abort::AbortController) -> Option<AbortLink> {
    let source = source?;
    if source.aborted() { target.abort(None); return None; }
    let target = target.clone();
    let listener = source.add_abort_listener(move |_|target.abort(None));
    Some(AbortLink { source: source.clone(), listener })
}

pub fn create_live_blocking_remote_compaction_event(
    context: &maho_ext_api::ExtensionContext,
    preparation: maho_ext_api::CompactionPreparation,
    custom_instructions: String,
    signal: maho_ext_api::AbortSignal,
) -> maho_ext_api::SessionBeforeCompactEvent {
    create_blocking_remote_compaction_event(context, preparation, maho_core::session_manager::create_session_id(), custom_instructions, signal)
}
