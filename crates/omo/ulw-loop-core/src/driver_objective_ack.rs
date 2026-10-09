//! Acknowledged driver objectives helper.

use crate::domain_types::UlwLoopPlan;

pub fn normalize_driver_objective(value: &str) -> String {
    let mut words = Vec::new();
    for word in value.split_whitespace() {
        words.push(word);
    }
    words.join(" ")
}

pub fn acknowledged_driver_objectives(plan: &UlwLoopPlan) -> &[String] {
    plan.acknowledged_driver_objectives.as_deref().unwrap_or(&[])
}

pub fn acknowledge_driver_objective(plan: &mut UlwLoopPlan, objective: Option<&str>) -> bool {
    let Some(obj) = objective else { return false };
    let normalized = normalize_driver_objective(obj);
    if normalized.is_empty() {
        return false;
    }
    let current = acknowledged_driver_objectives(plan);
    if current.iter().any(|item| item == &normalized) {
        return false;
    }
    let mut updated = current.to_vec();
    updated.push(normalized);
    plan.acknowledged_driver_objectives = Some(updated);
    true
}
