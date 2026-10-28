use maho_ai::types::Message;
use maho_core::compaction::compaction::estimate_tokens;
use sha2::{Digest, Sha256};
use crate::{context_reduction::{reduce_context_messages, should_apply_context_reduction, ReduceContextOptions}, overflow_retry::estimate_total_tokens};

pub use crate::context_reduction_state::{ContextReductionState, clear_reduction_anchor, create_context_reduction_state};

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

fn reduce_prefix(messages: &[Message], cut: usize) -> Vec<Message> {
    // senpi reducePrefix: the builtin options with only the recency protections zeroed.
    let builtin=ReduceContextOptions::builtin();
    let options=ReduceContextOptions {
        collapse:builtin.collapse.map(|options|crate::context_reduction::CollapseConsecutiveOptions {protect_recent_messages:0,..options}),
        shrink_assistant:builtin.shrink_assistant.map(|options|crate::context_reduction::MicroCompactAssistantOptions {protect_recent_tokens:0,..options}),
        clear_tool_results:builtin.clear_tool_results.map(|options|crate::context_reduction::ClearOldToolResultsOptions {keep_recent:0,..options}),
    };
    let mut prefix=reduce_context_messages(&messages[..cut],&options).messages;
    prefix.extend_from_slice(&messages[cut..]);
    prefix
}

/// Copy and mark the last assistant preceding each immutable frontier cut.
pub fn with_stable_prefix_checkpoints(messages: &[Message], state: &ContextReductionState) -> Vec<Message> {
    let mut marked = messages.to_vec();
    for cut in state.checkpoint_cut.into_iter().chain(std::iter::once(state.cut_index)) {
        for message in marked.iter_mut().take(cut).rev() {
            if let Message::Assistant(assistant) = message {
                assistant.prompt_cache_breakpoint = Some(true);
                break;
            }
        }
    }
    marked
}

/// The token budget a frontier reduction pass runs against.
#[derive(Clone, Copy, Debug)]
pub struct ReductionBudget { pub window:u64, pub ceiling:u64, pub overhead:u64, pub block_ratio:Option<f64>, pub force:bool }

pub fn reduce_context_with_frontier(messages:&[Message],unreduced:&[Message],state:&mut ContextReductionState,budget:ReductionBudget)->Vec<Message> {
    let ReductionBudget { window, ceiling, overhead, block_ratio, force } = budget;
    if state.cut_index>messages.len() || (state.cut_index>0 && fingerprint(unreduced,state.cut_index)!=state.prefix_hash) { *state=create_context_reduction_state(); }
    let reduced=if state.engaged {reduce_prefix(messages,state.cut_index)} else {messages.to_vec()};
    let projected=projected_reduction_tokens(&reduced,state,unreduced,overhead);
    let pressure=((ceiling as f64*0.95).max(0.0).floor()) as u64;
    let ceiling_pressure=projected>=pressure;
    let was_engaged=state.engaged;
    let should_reduce=should_apply_context_reduction(Some(estimate_total_tokens(&json_messages(unreduced)).saturating_add(overhead) as f64), window as f64, None, false);
    if !state.engaged && !force && !ceiling_pressure && !should_reduce {return messages.to_vec();}
    state.engaged=true;
    if state.compaction_required {return with_stable_prefix_checkpoints(&reduced,state);}
    let tail=estimate_total_tokens(&json_messages(&unreduced[state.cut_index.min(unreduced.len())..]));
    if was_engaged && !ceiling_pressure && (tail as f64)<=window as f64*block_ratio.unwrap_or(0.1) {return with_stable_prefix_checkpoints(&reduced,state);}
    let mut cut=messages.len().saturating_sub(5);
    let mut recent=0u64;
    for index in (0..messages.len()).rev() {
        if recent>=3000 {break;}
        recent=recent.saturating_add(unreduced.get(index).and_then(|message|serde_json::to_value(message).ok()).map_or(0,|value|estimate_tokens(&value)));
        cut=cut.min(index);
    }
    if cut<=state.cut_index {
        if !was_engaged {state.engaged=false;} else if ceiling_pressure {state.compaction_required=true;}
        return with_stable_prefix_checkpoints(&reduced,state);
    }
    if ceiling_pressure && (estimate_total_tokens(&json_messages(&unreduced[state.cut_index..cut])) as f64)<window as f64*0.1 {
        if !was_engaged {state.engaged=false;} else {state.compaction_required=true;}
        return with_stable_prefix_checkpoints(&reduced,state);
    }
    let candidate=reduce_prefix(messages,cut);
    let savings=estimate_total_tokens(&json_messages(&reduced)).saturating_sub(estimate_total_tokens(&json_messages(&candidate)));
    let candidate_projected=(estimate_total_tokens(&json_messages(&candidate)).saturating_add(overhead)).max(projected.saturating_sub(savings));
    if (savings as f64)<window as f64*0.02 || (ceiling_pressure && (candidate_projected as f64)>(ceiling as f64*0.85)) {
        if ceiling_pressure {state.compaction_required=true;}
        return with_stable_prefix_checkpoints(&reduced,state);
    }
    state.checkpoint_cut=(state.cut_index>0).then_some(state.cut_index);
    state.cut_index=cut;
    state.prefix_hash=fingerprint(unreduced,cut);
    clear_reduction_anchor(state);
    with_stable_prefix_checkpoints(&candidate,state)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_reduction_state::{read_context_reduction_state, snapshot_reduction_state};
    #[test]
    fn checkpoints_copy_old_and_current_prefix_and_round_trip_state() {
        let assistant = || Message::Assistant(Box::new(maho_ai::providers::faux::faux_assistant_message("reply", Default::default())));
        let messages = vec![assistant(), assistant(), assistant(), assistant()];
        let state = ContextReductionState { engaged:true, checkpoint_cut:Some(1),cut_index:3,..Default::default() };
        let marked = with_stable_prefix_checkpoints(&messages,&state);
        for (index,message) in marked.iter().enumerate() {
            let Message::Assistant(message)=message else {panic!("assistant")};
            assert_eq!(message.prompt_cache_breakpoint, matches!(index,0|2).then_some(true));
        }
        let Message::Assistant(original)=&messages[0] else {panic!("assistant")};
        assert_eq!(original.prompt_cache_breakpoint,None);
        let mut snapshot=snapshot_reduction_state(&state);
        // Optional anchor fields are absent, rather than an incomplete anchor.
        for key in ["anchorCount","anchorCut","anchorHash","anchorTokens"] {snapshot.as_object_mut().expect("snapshot").remove(key);}
        assert_eq!(read_context_reduction_state(&snapshot).expect("state").checkpoint_cut,Some(1));
        let next = ContextReductionState {checkpoint_cut:Some(3),cut_index:4,..state};
        let marked=with_stable_prefix_checkpoints(&messages,&next);
        let Message::Assistant(first)=&marked[0] else {panic!("assistant")};
        assert_eq!(first.prompt_cache_breakpoint,None);
    }
}
