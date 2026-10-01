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
