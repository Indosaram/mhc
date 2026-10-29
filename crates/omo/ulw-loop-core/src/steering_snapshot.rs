//! Steering snapshot builders.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain_types::{UlwLoopItem, UlwLoopPlan};
use crate::steering_types::UlwLoopSteeringPlanSnapshot;

pub fn build_steering_plan_snapshot(
    plan: &UlwLoopPlan,
    changed_goal_ids: &BTreeSet<String>,
) -> UlwLoopSteeringPlanSnapshot {
    UlwLoopSteeringPlanSnapshot {
        updated_at: plan.updated_at.clone(),
        active_goal_id: plan.active_goal_id.clone(),
        goal_count: plan.goals.len(),
        goal_ids: plan.goals.iter().map(|g| g.id.clone()).collect(),
        goals: plan
            .goals
            .iter()
            .filter(|g| changed_goal_ids.contains(&g.id))
            .cloned()
            .collect(),
    }
}

pub fn changed_goal_ids_between(before: &UlwLoopPlan, after: &UlwLoopPlan) -> BTreeSet<String> {
    let mut before_by_id = BTreeMap::new();
    for g in &before.goals {
        before_by_id.insert(g.id.clone(), g);
    }

    let mut changed = BTreeSet::new();
    for goal in &after.goals {
        match before_by_id.get(&goal.id) {
            None => {
                changed.insert(goal.id.clone());
            }
            Some(prior) => {
                let prior_json = serde_json::to_string(prior).unwrap_or_default();
                let goal_json = serde_json::to_string(goal).unwrap_or_default();
                if prior_json != goal_json {
                    changed.insert(goal.id.clone());
                }
            }
        }
        before_by_id.remove(&goal.id);
    }

    for id in before_by_id.keys() {
        changed.insert(id.clone());
    }

    changed
}
