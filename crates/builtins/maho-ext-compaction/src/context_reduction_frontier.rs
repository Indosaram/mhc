use maho_ai::types::Message;
use maho_core::compaction::compaction::estimate_tokens;
use sha2::{Digest, Sha256};
use crate::{context_reduction::{reduce_context_messages, should_apply_context_reduction, ReduceContextOptions}, overflow_retry::estimate_total_tokens};

#[derive(Clone, Debug, Default)]
pub struct ContextReductionState {
    pub engaged: bool,
    pub cut_index: usize,
    pub prefix_hash: String,
    pub anchor_count: Option<usize>,
    pub anchor_cut: Option<usize>,
    pub anchor_hash: Option<String>,
    pub anchor_tokens: Option<u64>,
    pub compaction_required: bool,
}

pub fn projected_reduction_tokens(messages: &[Message], state: &ContextReductionState, unreduced: &[Message], overhead: u64) -> u64 {
    let estimate = estimate_total_tokens(&json_messages(messages)).saturating_add(overhead);
    if state.anchor_count.is_none_or(|count|count>unreduced.len()) || state.anchor_tokens.is_none() || state.anchor_cut!=Some(state.cut_index) || state.anchor_hash.as_deref()!=Some(&state.prefix_hash) {
        return estimate;
    }
    estimate.max(state.anchor_tokens.unwrap_or(0).saturating_add(estimate_total_tokens(&json_messages(&unreduced[state.anchor_count.unwrap_or(0)..]))))
}

fn json_messages(messages: &[Message]) -> Vec<serde_json::Value> {
    messages.iter().filter_map(|message|serde_json::to_value(message).ok()).collect()
}

fn fingerprint(messages: &[Message], cut: usize) -> String {
    let mut digest=Sha256::new();
    for message in messages.iter().take(cut) {
        if let Ok(encoded)=serde_json::to_vec(message) { digest.update((encoded.len() as u64).to_le_bytes()); digest.update(encoded); }
    }
    format!("{:x}",digest.finalize())
}

pub fn read_context_reduction_state(value:&serde_json::Value)->Option<ContextReductionState> {
    let engaged=value.get("engaged")?.as_bool()?;
    let cut=value.get("cutIndex")?.as_u64()? as usize;
    let hash=value.get("prefixHash")?.as_str()?.to_owned();
    let mut state=ContextReductionState {engaged,cut_index:cut,prefix_hash:hash,..Default::default()};
    if value.get("anchorCount").is_some() || value.get("anchorCut").is_some() || value.get("anchorHash").is_some() || value.get("anchorTokens").is_some() {
        state.anchor_count=Some(value.get("anchorCount")?.as_u64()? as usize);
        state.anchor_cut=Some(value.get("anchorCut")?.as_u64()? as usize);
        state.anchor_hash=Some(value.get("anchorHash")?.as_str()?.to_owned());
        state.anchor_tokens=Some(value.get("anchorTokens")?.as_u64()?);
        if state.anchor_cut? > state.anchor_count? {return None;}
    }
    Some(state)
}

pub fn snapshot_reduction_state(state:&ContextReductionState)->serde_json::Value {
    serde_json::json!({"engaged":state.engaged,"cutIndex":state.cut_index,"prefixHash":state.prefix_hash,"anchorCount":state.anchor_count,"anchorCut":state.anchor_cut,"anchorHash":state.anchor_hash,"anchorTokens":state.anchor_tokens})
}

pub fn record_reduction_usage(state:&mut ContextReductionState,count:usize,cut:usize,hash:&str,tokens:f64) {
    if state.engaged && tokens.is_finite() && tokens>0. && !state.compaction_required && cut==state.cut_index && hash==state.prefix_hash {
        state.anchor_count=Some(count);state.anchor_cut=Some(cut);state.anchor_hash=Some(hash.to_owned());state.anchor_tokens=Some(tokens.ceil() as u64);
    }
}

fn reduce_prefix(messages: &[Message], cut: usize) -> Vec<Message> {
    let options=ReduceContextOptions {
        collapse:Some(crate::context_reduction::CollapseConsecutiveOptions {protect_recent_messages:0,..Default::default()}),
        shrink_assistant:Some(crate::context_reduction::MicroCompactAssistantOptions {protect_recent_tokens:0,..Default::default()}),
        clear_tool_results:Some(Default::default()),
    };
    let mut prefix=reduce_context_messages(&messages[..cut],&options).messages;
    prefix.extend_from_slice(&messages[cut..]);
    prefix
}

pub fn reduce_context_with_frontier(messages:&[Message],unreduced:&[Message],state:&mut ContextReductionState,window:u64,ceiling:u64,overhead:u64,block_ratio:Option<f64>,force:bool)->Vec<Message> {
    if state.cut_index>messages.len() || (state.cut_index>0 && fingerprint(unreduced,state.cut_index)!=state.prefix_hash) { *state=ContextReductionState::default(); }
    let reduced=if state.engaged {reduce_prefix(messages,state.cut_index)} else {messages.to_vec()};
    let projected=projected_reduction_tokens(&reduced,state,unreduced,overhead);
    let pressure=((ceiling as f64*0.95).max(0.0).floor()) as u64;
    let ceiling_pressure=projected>=pressure;
    let was_engaged=state.engaged;
    let should_reduce=should_apply_context_reduction(Some(estimate_total_tokens(&json_messages(unreduced)).saturating_add(overhead) as f64), window as f64, None, false);
    if !state.engaged && !force && !ceiling_pressure && !should_reduce {return messages.to_vec();}
    state.engaged=true;
    if state.compaction_required {return reduced;}
    let tail=estimate_total_tokens(&json_messages(&unreduced[state.cut_index.min(unreduced.len())..]));
    if was_engaged && !ceiling_pressure && (tail as f64)<=window as f64*block_ratio.unwrap_or(0.1) {return reduced;}
    let mut cut=messages.len().saturating_sub(5);
    let mut recent=0u64;
    for index in (0..messages.len()).rev() {
        if recent>=3000 {break;}
        recent=recent.saturating_add(unreduced.get(index).and_then(|message|serde_json::to_value(message).ok()).map_or(0,|value|estimate_tokens(&value)));
        cut=cut.min(index);
    }
    if cut<=state.cut_index {
        if !was_engaged {state.engaged=false;} else if ceiling_pressure {state.compaction_required=true;}
        return reduced;
    }
    if ceiling_pressure && (estimate_total_tokens(&json_messages(&unreduced[state.cut_index..cut])) as f64)<window as f64*0.1 {
        if !was_engaged {state.engaged=false;} else {state.compaction_required=true;}
        return reduced;
    }
    let candidate=reduce_prefix(messages,cut);
    let savings=estimate_total_tokens(&json_messages(&reduced)).saturating_sub(estimate_total_tokens(&json_messages(&candidate)));
    let candidate_projected=(estimate_total_tokens(&json_messages(&candidate)).saturating_add(overhead)).max(projected.saturating_sub(savings));
    if (savings as f64)<window as f64*0.02 || (ceiling_pressure && (candidate_projected as f64)>(ceiling as f64*0.85)) {
        if ceiling_pressure {state.compaction_required=true;}
        return reduced;
    }
    state.cut_index=cut;
    state.prefix_hash=fingerprint(unreduced,cut);
    state.anchor_count=None;state.anchor_cut=None;state.anchor_hash=None;state.anchor_tokens=None;
    candidate
}

