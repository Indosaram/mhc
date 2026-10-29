use super::{
    super::item::{ItemError, active_registers, render::render_doc_content},
    adopt::{AdoptionPlan, RetainedSource, Snapshot},
    read_back::{first_missing_fact, normalize_input},
};
use loro::LoroDoc;

#[derive(Debug, PartialEq, Eq)]
pub struct RemovalDecision {
    pub remove: Vec<String>,
    pub retained: Vec<RetainedSource>,
}

/// Call only after the adoption update is imported AND the item is durably persisted.
/// This gate inspects the committed item, not the planner's prediction; it never deletes files.
pub fn confirm_removals(
    doc: &LoroDoc,
    plan: &AdoptionPlan,
    files: &[Snapshot],
) -> Result<RemovalDecision, ItemError> {
    let committed = render_doc_content(doc)?;
    let registers = active_registers(doc);
    let mut decision = RemovalDecision {
        remove: Vec::new(),
        retained: plan.retained_sources.clone(),
    };
    for path in &plan.removal_candidates {
        let missing = match files.iter().find(|f| &f.path == path) {
            Some(source) => {
                first_missing_fact(&committed, &normalize_input(&source.text), &registers)
            }
            None => Some("source not among the adopted files".into()),
        };
        match missing {
            None => decision.remove.push(path.clone()),
            Some(reason) => decision.retained.push(RetainedSource {
                path: path.clone(),
                reason,
            }),
        }
    }
    Ok(decision)
}
