use crate::model_usability_budget::ModelUsabilityBudgetProjection;

#[derive(Clone, Debug)]
pub struct ResumeCompactionRequirement { pub projection: ModelUsabilityBudgetProjection, pub notice: String }
pub fn create_resume_compaction_requirement(projection: ModelUsabilityBudgetProjection) -> ResumeCompactionRequirement {
    let notice = format!("Context exceeds the model budget by {} tokens; compacting before the first prompt.", projection.shortfall_tokens);
    ResumeCompactionRequirement { projection, notice }
}
