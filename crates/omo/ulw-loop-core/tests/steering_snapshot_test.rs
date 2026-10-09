use std::collections::BTreeSet;
use ulw_loop_core::domain_types::{UlwLoopItem, UlwLoopPlan};
use ulw_loop_core::steering_snapshot::{build_steering_plan_snapshot, changed_goal_ids_between};

const NOW: &str = "2026-05-23T00:00:00.000Z";
fn goal(id: &str) -> UlwLoopItem { UlwLoopItem { id:id.into(), title:format!("Goal {id}"), objective:format!("Objective for {id}"), status:"pending".into(), success_criteria:vec![], attempt:0, created_at:NOW.into(), updated_at:NOW.into(), started_at:None, completed_at:None, failed_at:None, review_blocked_at:None, evidence:None, failure_reason:None, steering_status:None, superseded_by:None, supersedes:None, blocked_reason:None, blocker_signature:None, blocker_occurrence_count:None, required_external_decision:None, non_retriable:None, steering_evidence:None, steering_rationale:None } }
fn plan(goals: Vec<UlwLoopItem>, active: Option<&str>) -> UlwLoopPlan { UlwLoopPlan { version:1, revision:None, codex_goal_mode:None, brief:None, ledger_reset_revision:None, evidence_layout_version:None, goals_path:".omo/ulw-loop/goals.json".into(), ledger_path:".omo/ulw-loop/ledger.jsonl".into(), brief_path:".omo/ulw-loop/brief.md".into(), created_at:NOW.into(), updated_at:NOW.into(), active_goal_id:active.map(Into::into), goals, codex_objective:None, codex_objective_aliases:None, acknowledged_driver_objectives:None, aggregate_completion:None, validation_batches:None } }

#[test] fn changed_goal_ids_cover_all_cases() {
    assert_eq!(changed_goal_ids_between(&plan(vec![goal("G001")],None),&plan(vec![goal("G001"),goal("G002")],None)), BTreeSet::from(["G002".into()]));
    assert_eq!(changed_goal_ids_between(&plan(vec![goal("G001"),goal("G002")],None),&plan(vec![goal("G001")],None)), BTreeSet::from(["G002".into()]));
    let b=goal("G002"); let mut a=b.clone(); a.title="Rewritten".into(); assert_eq!(changed_goal_ids_between(&plan(vec![goal("G001"),b],None),&plan(vec![goal("G001"),a],None)), BTreeSet::from(["G002".into()]));
    assert!(changed_goal_ids_between(&plan(vec![goal("G001"),goal("G002"),goal("G003")],None),&plan(vec![goal("G003"),goal("G001"),goal("G002")],None)).is_empty());
    assert!(changed_goal_ids_between(&plan(vec![goal("G001"),goal("G002")],None),&plan(vec![goal("G001"),goal("G002")],None)).is_empty());
    let mut changed=goal("G001"); changed.status="blocked".into(); assert_eq!(changed_goal_ids_between(&plan(vec![goal("G001"),goal("G002"),goal("G003")],None),&plan(vec![changed,goal("G003"),goal("G004")],None)), BTreeSet::from(["G001".into(),"G002".into(),"G004".into()]));
}
#[test] fn snapshot_keeps_changed_goals_and_order() { let source=plan(vec![goal("G001"),goal("G002"),goal("G003"),goal("G004")],None); let snapshot=build_steering_plan_snapshot(&source,&BTreeSet::from(["G003".into()])); assert_eq!(snapshot.goals.iter().map(|g|g.id.as_str()).collect::<Vec<_>>(),vec!["G003"]); assert_eq!(snapshot.goal_ids,vec!["G001","G002","G003","G004"]); assert_eq!(snapshot.goal_count,4); assert_eq!(snapshot.updated_at,NOW); }
#[test] fn snapshot_optional_active_and_order_cases() {
    assert_eq!(build_steering_plan_snapshot(&plan(vec![goal("G001")],None),&BTreeSet::from(["G001".into()])).active_goal_id,None);
    assert_eq!(build_steering_plan_snapshot(&plan(vec![goal("G001")],Some("G001")),&BTreeSet::new()).active_goal_id,Some("G001".into()));
    let source=plan(vec![goal("G001"),goal("G002"),goal("G003")],None); let snapshot=build_steering_plan_snapshot(&source,&BTreeSet::from(["G003".into(),"G001".into()])); assert_eq!(snapshot.goals.iter().map(|g|g.id.as_str()).collect::<Vec<_>>(),vec!["G001","G003"]);
}
#[test] fn snapshot_changed_goal_is_exact_source_value() { let source=plan(vec![goal("G001"),goal("G002")],None); let snapshot=build_steering_plan_snapshot(&source,&BTreeSet::from(["G002".into()])); assert_eq!(snapshot.goals[0].id,source.goals[1].id); assert_eq!(snapshot.goals[0].title,source.goals[1].title); }
