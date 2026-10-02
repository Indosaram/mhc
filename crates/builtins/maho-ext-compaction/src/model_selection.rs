use maho_ai::types::Model;
use maho_core::compaction::settings::CompactionSettings;
use crate::{state::CompactionExtensionState,orchestration::resolve_compaction_geometry,policy::should_start_speculative_compaction};
pub struct ModelSelectionInput<'a> {
    pub previous_model:Option<&'a Model>,pub selected_model:Option<&'a Model>,pub speculative_model:Option<&'a Model>,
    pub state:&'a CompactionExtensionState,pub lane_owns_compaction:bool,pub breaker_tripped:bool,
    pub usage_tokens:Option<f64>,pub settings:&'a CompactionSettings,
}
pub fn handle_compaction_model_select(input:ModelSelectionInput<'_>,mut invalidate:impl FnMut(),mut start:impl FnMut()) {
    if input.lane_owns_compaction {invalidate();return;}
    let matches=match (input.speculative_model,input.selected_model) {
        (Some(job),Some(selected)) => job.api==selected.api && job.provider==selected.provider && job.id==selected.id && job.base_url==selected.base_url && job.context_window==selected.context_window,
        _ => false,
    };
    if !matches {invalidate();}
    let previous=input.previous_model.map_or(0,|m|m.context_window);
    let window=input.selected_model.map_or(0,|m|m.context_window);
    if previous<=window || input.breaker_tripped || input.usage_tokens.is_none() {return;}
    let geometry=resolve_compaction_geometry(window as f64,input.settings,input.state.last_yield);
    if should_start_speculative_compaction(input.usage_tokens,window as f64,input.settings,input.state.last_yield,Some(geometry.lead_tokens)) {start();}
}

pub fn handle_live_compaction_model_select(
    event: &maho_ext_api::ModelSelectEvent,
    context: &maho_ext_api::ExtensionContext,
    state: &CompactionExtensionState,
    speculative: Option<&crate::speculative::SpeculativeCompactionSnapshot>,
    settings: &CompactionSettings,
    lane_owns_compaction: bool,
    callbacks: (impl FnMut(), impl FnMut()),
) -> Result<(), maho_ext_api::ExtensionFailure> {
    let breaker_tripped = crate::circuit_breaker::is_tripped(state,chrono::Utc::now().timestamp_millis() as f64);
    let shrank = event.previous_model.as_ref().map_or(0,|model|model.context_window) > context.model.as_ref().map_or(0,|model|model.context_window);
    let usage_tokens = if lane_owns_compaction || breaker_tripped || !shrank { None } else { context.get_context_usage()?.and_then(|usage|usage.tokens).map(|tokens|tokens as f64) };
    handle_compaction_model_select(ModelSelectionInput {
        previous_model: event.previous_model.as_ref(), selected_model: context.model.as_ref(), speculative_model: speculative.map(|snapshot|&snapshot.model),
        state, lane_owns_compaction, breaker_tripped, usage_tokens, settings,
    },callbacks.0,callbacks.1);
    Ok(())
}
