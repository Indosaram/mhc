use maho_ai::types::Model;
use crate::model_usability_budget::ModelUsabilityBudgetProjection;

#[derive(Clone, Debug)]
pub struct PendingModelSwitch {
    pub model: Model,
    pub projection: ModelUsabilityBudgetProjection,
    pub persist_default: bool,
    pub notice: String,
}
pub fn create_pending_model_switch(model: Model, projection: ModelUsabilityBudgetProjection, persist_default: bool) -> PendingModelSwitch {
    let notice = format!("{} needs {} fewer tokens than this conversation holds. It is compacted on your next message, and the switch applies after that.", model.id, projection.shortfall_tokens);
    PendingModelSwitch { model, projection, persist_default, notice }
}
pub fn pending_switch_keep_recent_tokens(projection: &ModelUsabilityBudgetProjection) -> f64 {
    let overhead = projection.required_tokens - projection.live_context_tokens;
    (projection.post_compaction_required_tokens - overhead).max(1.0)
}
