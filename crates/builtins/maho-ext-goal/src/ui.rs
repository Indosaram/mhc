use maho_ext_api::ExtensionContext;
use crate::{format::format_goal_elapsed_seconds,types::{Goal,GoalStatus},validation::js_whitespace};
pub const STATUS_KEY:&str="goal";
pub const OBJECTIVE_PREVIEW_MAX_LENGTH:usize=32;
pub fn truncate_goal_objective(objective:&str,max_length:usize)->String {
    let normalized=objective.split(js_whitespace).filter(|part|!part.is_empty()).collect::<Vec<_>>().join(" ");
    if normalized.encode_utf16().count()<=max_length { return normalized; }
    let truncated=String::from_utf16_lossy(&normalized.encode_utf16().take(max_length.saturating_sub(1)).collect::<Vec<_>>()); format!("{}\u{2026}",truncated.trim_end_matches(js_whitespace))
}
pub fn goal_status_text(goal:&Goal,live_elapsed_seconds:Option<f64>)->String {
    match goal.status {
        GoalStatus::Active=>if let Some(seconds)=live_elapsed_seconds { format!("Pursuing goal ({})",format_goal_elapsed_seconds(seconds)) } else if goal.time_used_seconds>0.0 { format!("Pursuing goal ({})",format_goal_elapsed_seconds(goal.time_used_seconds)) } else { "Pursuing goal".into() },
        GoalStatus::Paused=>"Goal paused (/goal resume)".into(),
        GoalStatus::Blocked=>goal.blocked_reason.as_ref().filter(|reason|!reason.is_empty()).map_or_else(||"Goal blocked".into(),|reason|format!("Goal blocked: {reason}")),
        GoalStatus::Complete=>{ let elapsed=if goal.time_used_seconds>0.0 { format!(" ({})",format_goal_elapsed_seconds(goal.time_used_seconds)) } else { String::new() }; format!("{} \u{b7} Goal achieved{elapsed}",truncate_goal_objective(&goal.objective,OBJECTIVE_PREVIEW_MAX_LENGTH)) },
    }
}
pub fn update_goal_ui(ctx:&ExtensionContext,goal:Option<&Goal>,live_elapsed_seconds:Option<f64>) { if !ctx.has_ui { return; } let status=goal.map(|goal|goal_status_text(goal,live_elapsed_seconds)); ctx.ui.set_status(STATUS_KEY,status.as_deref()); }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn upstream_status_surface_sets_clears_and_respects_headless_context() {
        let ui=std::sync::Arc::new(crate::test_context::Ui::default()); let mut context=crate::test_context::context(); context.ui=ui.clone();
        update_goal_ui(&context,Some(&goal(GoalStatus::Active,0.0)),None); update_goal_ui(&context,None,None);
        let calls=ui.statuses.lock().unwrap(); assert_eq!(calls.len(),2); assert_eq!(calls[0].0,STATUS_KEY); assert!(calls[0].1.is_some()); assert_eq!(calls[1],(STATUS_KEY.into(),None)); drop(calls);
        context.has_ui=false; update_goal_ui(&context,Some(&goal(GoalStatus::Active,0.0)),None); assert_eq!(ui.statuses.lock().unwrap().len(),2);
    }
    fn goal(status:GoalStatus,seconds:f64)->Goal { serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"Ship the feature","status":status,"tokensUsed":0,"timeUsedSeconds":seconds,"createdAt":1,"updatedAt":1})).unwrap() }
    #[test] fn upstream_footer_derives_each_goal_state() {
        for (status,seconds,expected) in [(GoalStatus::Active,0.0,"Pursuing goal"),(GoalStatus::Active,65.0,"Pursuing goal (1m)"),(GoalStatus::Paused,0.0,"Goal paused (/goal resume)"),(GoalStatus::Complete,0.0,"Ship the feature \u{b7} Goal achieved"),(GoalStatus::Complete,125.0,"Ship the feature \u{b7} Goal achieved (2m)")] { assert_eq!(goal_status_text(&goal(status,seconds),None),expected); }
        let mut blocked=goal(GoalStatus::Blocked,0.0); blocked.blocked_reason=Some("Waiting for review".into()); assert_eq!(goal_status_text(&blocked,None),"Goal blocked: Waiting for review");
    }
    #[test] fn upstream_achieved_footer_truncates_objective_to_32_units() {
        let mut achieved=goal(GoalStatus::Complete,61.0); achieved.objective="Refactor the entire session persistence layer to support branching".into();
        let preview=truncate_goal_objective(&achieved.objective,OBJECTIVE_PREVIEW_MAX_LENGTH); assert!(preview.ends_with('\u{2026}')); assert!(preview.encode_utf16().count()<=32); assert_eq!(goal_status_text(&achieved,None),format!("{preview} \u{b7} Goal achieved (1m)"));
    }
    #[test] fn upstream_live_elapsed_only_applies_to_active_goal() {
        assert_eq!(goal_status_text(&goal(GoalStatus::Active,5.0),Some(42.0)),"Pursuing goal (42s)");
        assert_eq!(goal_status_text(&goal(GoalStatus::Active,0.0),Some(0.0)),"Pursuing goal (0s)");
        assert_eq!(goal_status_text(&goal(GoalStatus::Paused,0.0),Some(99.0)),"Goal paused (/goal resume)");
        assert_eq!(goal_status_text(&goal(GoalStatus::Complete,0.0),Some(99.0)),"Ship the feature \u{b7} Goal achieved");
    }
    #[test] fn preview_normalizes_ecmascript_whitespace() { assert_eq!(truncate_goal_objective("\u{feff} ship\n  the\tfeature \u{feff}",32),"ship the feature"); }
}
