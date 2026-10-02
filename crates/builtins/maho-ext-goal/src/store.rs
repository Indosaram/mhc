use crate::{errors::GoalError, persistence::{encoded_thread_id, read_goal_file, write_goal_file}, transitions::transition_goal_status, types::{Goal, GoalAccountingMode, GoalStatus, GoalStoreRef, GoalUpdate, GoalUpdateSource, TokenUsageSnapshot}, validation::{resolve_token_budget, validate_objective, validate_token_budget}};
use maho_core::session_sidecar_store::serialize_by_key;
use std::{fs, io::Write, path::PathBuf};
pub use crate::persistence::goal_file_path;
pub fn goal_history_file_path(reference: &GoalStoreRef) -> PathBuf { reference.base_dir.join(format!("{}.history.jsonl", encoded_thread_id(reference))) }
pub fn objective_full_text_file_name(reference: &GoalStoreRef) -> String { format!("{}.objective-full.txt", encoded_thread_id(reference)) }
pub fn objective_full_text_file_path(reference: &GoalStoreRef) -> PathBuf { reference.base_dir.join(objective_full_text_file_name(reference)) }
pub fn read_goal(reference: &GoalStoreRef) -> Result<Option<Goal>, GoalError> { read_goal_file(reference) }
pub async fn write_goal(reference: &GoalStoreRef, goal: Option<&Goal>) -> Result<(), GoalError> {
    serialize_by_key(&goal_file_path(reference).to_string_lossy(), async { write_goal_file(reference, goal) }).await
}
fn fresh_goal(reference: &GoalStoreRef, objective: String, status: GoalStatus, token_budget: Option<u64>, now: u64) -> Goal {
    Goal { id: uuid::Uuid::new_v4().to_string(), thread_id: reference.thread_id.clone(), objective, status, token_budget, tokens_used: 0, time_used_seconds: 0.0, consecutive_continuations: Some(0), unattended_continuations: Some(0), last_continuation_signature: None, created_at: now, updated_at: now, last_started_at: (status == GoalStatus::Active).then_some(now), blocked_reason: None, blocked_at: None, completed_at: (status == GoalStatus::Complete).then_some(now) }
}
pub async fn create_goal(reference: &GoalStoreRef, objective: &str, token_budget: Option<u64>, now: u64) -> Result<Goal, GoalError> {
    serialize_by_key(&goal_file_path(reference).to_string_lossy(), async {
        let validated = validate_objective(objective, &objective_full_text_file_name(reference))?;
        let current = read_goal_file(reference)?;
        if current.as_ref().is_some_and(|g| g.status != GoalStatus::Complete) { return Err(GoalError::AlreadyExists("cannot create a new goal because this thread already has a goal".into())); }
        if validated.truncated { write_full_objective_text(reference, objective)?; }
        if let Some(current) = current { archive_goal(reference, &current)?; }
        let goal = fresh_goal(reference, validated.objective, GoalStatus::Active, token_budget.map(validate_token_budget).transpose()?, now);
        write_goal_file(reference, Some(&goal))?;
        Ok(goal)
    }).await
}
pub async fn update_goal(reference: &GoalStoreRef, update: &GoalUpdate, source: GoalUpdateSource, now: u64) -> Result<Goal, GoalError> {
    serialize_by_key(&goal_file_path(reference).to_string_lossy(), async {
        let current = read_goal_file(reference)?.ok_or_else(|| GoalError::NotFound("cannot update goal: no goal exists".into()))?;
        let validated = update.objective.as_ref().map(|value| validate_objective(value, &objective_full_text_file_name(reference))).transpose()?;
        let objective = validated.as_ref().map_or_else(|| current.objective.clone(), |v| v.objective.clone());
        let token_budget = resolve_token_budget(current.token_budget, update.token_budget)?;
        let now = now.max(current.updated_at.saturating_add(1));
        let replaces = update.objective.is_some() && (objective != current.objective || current.status == GoalStatus::Complete);
        let requested_status = update.status.or_else(|| update.objective.as_ref().map(|_| GoalStatus::Active));
        let next = if replaces {
            let status = requested_status.unwrap_or(GoalStatus::Active);
            if status == GoalStatus::Blocked { return Err(GoalError::InvalidMutation("objective replacement cannot create a blocked goal".into())); }
            fresh_goal(reference, objective, status, token_budget, now)
        } else {
            let previous_status = current.status;
            let mut current = current;
            current.objective = objective;
            let mut next = transition_goal_status(&current, requested_status.unwrap_or(current.status), source, update.reason.as_deref(), now)?;
            if next.status != previous_status { next.consecutive_continuations = Some(0); next.unattended_continuations = Some(0); next.last_continuation_signature = None; }
            next.token_budget = token_budget;
            next
        };
        if validated.as_ref().is_some_and(|v| v.truncated) { write_full_objective_text(reference, update.objective.as_deref().unwrap_or(""))?; }
        write_goal_file(reference, Some(&next))?;
        Ok(next)
    }).await
}
pub fn archive_goal(reference: &GoalStoreRef, goal: &Goal) -> Result<(), GoalError> {
    fs::create_dir_all(&reference.base_dir).map_err(|e| GoalError::Io(e.to_string()))?;
    let line = serde_json::to_string(goal).map_err(|e| GoalError::Json(e.to_string()))?;
    let mut file = fs::OpenOptions::new().create(true).append(true).open(goal_history_file_path(reference)).map_err(|e| GoalError::Io(e.to_string()))?;
    writeln!(file, "{line}").map_err(|e| GoalError::Io(e.to_string()))
}
fn write_full_objective_text(reference: &GoalStoreRef, objective: &str) -> Result<(), GoalError> {
    fs::create_dir_all(&reference.base_dir).and_then(|()| fs::write(objective_full_text_file_path(reference), objective)).map_err(|e| GoalError::Io(e.to_string()))
}
pub async fn clear_goal(reference: &GoalStoreRef) -> Result<bool, GoalError> {
    serialize_by_key(&goal_file_path(reference).to_string_lossy(), async { let existed = read_goal_file(reference)?.is_some(); write_goal_file(reference, None)?; Ok(existed) }).await
}
pub async fn account_goal_usage(reference: &GoalStoreRef, usage: &TokenUsageSnapshot, elapsed_seconds: f64, mode: GoalAccountingMode, expected_goal_id: Option<&str>, now: u64) -> Result<Option<Goal>, GoalError> {
    serialize_by_key(&goal_file_path(reference).to_string_lossy(), async {
        let Some(mut goal) = read_goal_file(reference)? else { return Ok(None); };
        let can_account = match mode { GoalAccountingMode::Active => goal.status == GoalStatus::Active, GoalAccountingMode::ActiveOrBlocked => matches!(goal.status, GoalStatus::Active | GoalStatus::Blocked), GoalAccountingMode::ActiveOrComplete => matches!(goal.status, GoalStatus::Active | GoalStatus::Complete) };
        if expected_goal_id.is_some_and(|id| id != goal.id) || !can_account { return Ok(Some(goal)); }
        goal.tokens_used = goal.tokens_used.saturating_add(usage.input).saturating_add(usage.output);
        goal.time_used_seconds += elapsed_seconds.trunc().max(0.0);
        goal.updated_at = now.max(goal.updated_at.saturating_add(1));
        write_goal_file(reference, Some(&goal))?;
        Ok(Some(goal))
    }).await
}
pub async fn record_continuation_delivered(reference: &GoalStoreRef, signature: &str, expected_goal_id: Option<&str>, count_unattended: bool) -> Result<Option<Goal>, GoalError> {
    serialize_by_key(&goal_file_path(reference).to_string_lossy(), async {
        let Some(mut goal) = read_goal_file(reference)? else { return Ok(None); };
        if expected_goal_id.is_some_and(|id| id != goal.id) { return Ok(None); }
        goal.consecutive_continuations = Some(goal.consecutive_continuations.unwrap_or(0).saturating_add(1));
        goal.unattended_continuations = Some(goal.unattended_continuations.unwrap_or(0).saturating_add(u64::from(count_unattended)));
        goal.last_continuation_signature = Some(signature.into());
        write_goal_file(reference, Some(&goal))?;
        Ok(Some(goal))
    }).await
}
pub async fn reset_continuation_streak(reference: &GoalStoreRef, unattended: bool) -> Result<Option<Goal>, GoalError> {
    serialize_by_key(&goal_file_path(reference).to_string_lossy(), async {
        let Some(mut goal) = read_goal_file(reference)? else { return Ok(None); };
        goal.consecutive_continuations = Some(0);
        if unattended { goal.unattended_continuations = Some(0); }
        goal.last_continuation_signature = None;
        write_goal_file(reference, Some(&goal))?;
        Ok(Some(goal))
    }).await
}
#[cfg(test)] mod tests {
    use super::*;
    fn reference(dir: &tempfile::TempDir) -> GoalStoreRef { GoalStoreRef { base_dir: dir.path().join("goal"), thread_id: "thread".into() } }
    #[tokio::test] async fn upstream_status_updates_preserve_usage_and_replacement_clears_it() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); let original=create_goal(&reference,"Original",None,1).await.unwrap();
        account_goal_usage(&reference,&TokenUsageSnapshot { input:23,output:2,cache_read:0,cache_write:4,total_tokens:25 },70.0,GoalAccountingMode::Active,None,2).await.unwrap();
        let paused=update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Paused),..Default::default() },GoalUpdateSource::User,3).await.unwrap(); assert_eq!(paused.id,original.id); assert_eq!(paused.tokens_used,25); assert_eq!(paused.time_used_seconds,70.0);
        let replaced=update_goal(&reference,&GoalUpdate { objective:Some("Replacement".into()),..Default::default() },GoalUpdateSource::User,4).await.unwrap(); assert_ne!(replaced.id,original.id); assert_eq!(replaced.tokens_used,0); assert_eq!(replaced.time_used_seconds,0.0); assert_eq!(replaced.status,GoalStatus::Active);
    }
    #[tokio::test] async fn upstream_new_goal_and_objective_replacement_reset_all_tracking() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); let original=create_goal(&reference,"First",None,1).await.unwrap();
        assert_eq!(original.consecutive_continuations,Some(0)); assert_eq!(original.unattended_continuations,Some(0)); assert!(original.last_continuation_signature.is_none());
        record_continuation_delivered(&reference,"signature",Some(&original.id),true).await.unwrap();
        let next=update_goal(&reference,&GoalUpdate { objective:Some("Second".into()),..Default::default() },GoalUpdateSource::User,2).await.unwrap();
        assert_ne!(next.id,original.id); assert_eq!(next.consecutive_continuations,Some(0)); assert_eq!(next.unattended_continuations,Some(0)); assert!(next.last_continuation_signature.is_none());
    }
    #[tokio::test] async fn upstream_clear_preserves_versioned_null_envelope() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); create_goal(&reference,"Work",None,1).await.unwrap(); clear_goal(&reference).await.unwrap();
        let envelope:serde_json::Value=serde_json::from_str(&fs::read_to_string(goal_file_path(&reference)).unwrap()).unwrap(); assert_eq!(envelope,serde_json::json!({"version":1,"goal":null}));
    }
    #[tokio::test] async fn upstream_delivery_roundtrip_keeps_signature_and_both_counters() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); let original=create_goal(&reference,"Work",None,1).await.unwrap();
        record_continuation_delivered(&reference,"first",Some(&original.id),true).await.unwrap(); let delivered=record_continuation_delivered(&reference,"second",Some(&original.id),true).await.unwrap().unwrap();
        assert_eq!(read_goal(&reference).unwrap(),Some(delivered.clone())); assert_eq!(delivered.consecutive_continuations,Some(2)); assert_eq!(delivered.unattended_continuations,Some(2)); assert_eq!(delivered.last_continuation_signature.as_deref(),Some("second"));
    }
    #[tokio::test] async fn upstream_replaced_goal_rejects_stale_continuation_admission() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); let original=create_goal(&reference,"Original",None,1).await.unwrap();
        let replacement=update_goal(&reference,&GoalUpdate { objective:Some("Replacement".into()),..Default::default() },GoalUpdateSource::User,2).await.unwrap();
        assert!(record_continuation_delivered(&reference,"stale",Some(&original.id),true).await.unwrap().is_none()); assert_eq!(read_goal(&reference).unwrap(),Some(replacement));
    }
    #[tokio::test] async fn upstream_completion_stamps_time_and_user_resume_retains_identity() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); let first=create_goal(&reference,"Finish",None,1).await.unwrap();
        let completed=update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Complete),..Default::default() },GoalUpdateSource::Model,2).await.unwrap();
        assert_eq!(completed.completed_at,Some(2));
        assert!(update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Paused),..Default::default() },GoalUpdateSource::User,3).await.is_err());
        let resumed=update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Active),..Default::default() },GoalUpdateSource::User,3).await.unwrap();
        assert_eq!(resumed.id,first.id); assert_eq!(resumed.status,GoalStatus::Active); assert!(resumed.completed_at.is_none());
    }
    #[tokio::test] async fn upstream_matching_objective_resumes_same_nonterminal_identity() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); let first=create_goal(&reference,"Same",None,1).await.unwrap();
        let paused=update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Paused),..Default::default() },GoalUpdateSource::User,2).await.unwrap();
        let resumed=update_goal(&reference,&GoalUpdate { objective:Some("Same".into()),..Default::default() },GoalUpdateSource::User,3).await.unwrap();
        assert_eq!(paused.id,first.id); assert_eq!(resumed.id,first.id); assert_eq!(resumed.status,GoalStatus::Active);
    }
    #[tokio::test] async fn upstream_large_accounting_never_changes_status() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); create_goal(&reference,"Tracked",None,1).await.unwrap();
        let usage=TokenUsageSnapshot { input:10_000_000,output:0,cache_read:0,cache_write:0,total_tokens:10_000_000 };
        let goal=account_goal_usage(&reference,&usage,4.0,GoalAccountingMode::Active,None,2).await.unwrap().unwrap();
        assert_eq!(goal.status,GoalStatus::Active); assert_eq!(goal.tokens_used,10_000_000); assert_eq!(goal.time_used_seconds,4.0);
    }
    #[tokio::test] async fn upstream_paused_goal_does_not_account_active_usage() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); create_goal(&reference,"Tracked",None,1).await.unwrap();
        update_goal(&reference,&GoalUpdate { status:Some(GoalStatus::Paused),..Default::default() },GoalUpdateSource::User,2).await.unwrap();
        let usage=TokenUsageSnapshot { input:25,output:0,cache_read:0,cache_write:0,total_tokens:25 };
        let goal=account_goal_usage(&reference,&usage,3.0,GoalAccountingMode::Active,None,3).await.unwrap().unwrap();
        assert_eq!(goal.status,GoalStatus::Paused); assert_eq!(goal.tokens_used,0); assert_eq!(goal.time_used_seconds,0.0);
    }
    #[tokio::test] async fn upstream_delivery_and_reset_preserve_updated_at() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir); let original=create_goal(&reference,"Steady",None,7).await.unwrap();
        let delivered=record_continuation_delivered(&reference,"signature",Some(&original.id),true).await.unwrap().unwrap();
        assert_eq!(delivered.updated_at,original.updated_at); assert_eq!(read_goal(&reference).unwrap().unwrap().updated_at,original.updated_at);
        let reset=reset_continuation_streak(&reference,true).await.unwrap().unwrap(); assert_eq!(reset.updated_at,original.updated_at); assert_eq!(reset.consecutive_continuations,Some(0)); assert!(reset.last_continuation_signature.is_none());
    }
    #[tokio::test] async fn upstream_absent_goal_streak_helpers_return_none() {
        let dir=tempfile::tempdir().unwrap(); let reference=reference(&dir);
        assert!(record_continuation_delivered(&reference,"signature",None,true).await.unwrap().is_none()); assert!(reset_continuation_streak(&reference,true).await.unwrap().is_none());
    }
    #[tokio::test] async fn create_persists_active_goal_without_budget() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); let result = create_goal(&reference, "  Ship it  ", None, 1).await.unwrap(); assert_eq!(result.status, GoalStatus::Active); assert_eq!(result.objective, "Ship it"); assert_eq!(result.token_budget, None); assert_eq!(read_goal(&reference).unwrap(), Some(result)); }
    #[tokio::test] async fn second_create_preserves_unfinished_goal() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); let first = create_goal(&reference, "Original", None, 1).await.unwrap(); let result = create_goal(&reference, "Replacement", None, 2).await; assert!(matches!(result, Err(GoalError::AlreadyExists(_)))); assert_eq!(read_goal(&reference).unwrap().unwrap().id, first.id); }
    #[tokio::test] async fn changed_objective_replaces_identity_and_usage() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); let first = create_goal(&reference, "Original", None, 1).await.unwrap(); let result = update_goal(&reference, &GoalUpdate { objective: Some("New".into()), ..Default::default() }, GoalUpdateSource::User, 2).await.unwrap(); assert_ne!(result.id, first.id); assert_eq!(result.tokens_used, 0); }
    #[tokio::test] async fn usage_excludes_cached_tokens_and_truncates_elapsed_time() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); create_goal(&reference, "Work", None, 1).await.unwrap(); let usage = TokenUsageSnapshot { input: 23, output: 2, cache_read: 100, cache_write: 4, total_tokens: 129 }; let result = account_goal_usage(&reference, &usage, 70.8, GoalAccountingMode::Active, None, 2).await.unwrap().unwrap(); assert_eq!(result.tokens_used, 25); assert!((result.time_used_seconds - 70.0).abs() < f64::EPSILON); }
    #[tokio::test] async fn stale_goal_id_does_not_charge_new_goal() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); let original = create_goal(&reference, "Work", None, 1).await.unwrap(); let usage = TokenUsageSnapshot { input: 1, output: 1, cache_read: 0, cache_write: 0, total_tokens: 2 }; let result = account_goal_usage(&reference, &usage, 1.0, GoalAccountingMode::Active, Some("stale"), 2).await.unwrap().unwrap(); assert_eq!(result, original); }
    #[tokio::test] async fn completed_goal_is_archived_before_create_replacement() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); create_goal(&reference, "First", None, 1).await.unwrap(); update_goal(&reference, &GoalUpdate { status: Some(GoalStatus::Complete), ..Default::default() }, GoalUpdateSource::Model, 2).await.unwrap(); let result = create_goal(&reference, "Second", None, 3).await.unwrap(); let archive = fs::read_to_string(goal_history_file_path(&reference)).unwrap(); let archived: Goal = serde_json::from_str(archive.trim()).unwrap(); assert_eq!(archived.objective, "First"); assert_eq!(result.objective, "Second"); }
    #[tokio::test] async fn delivery_counts_can_exempt_unattended_waits() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); create_goal(&reference, "Work", None, 1).await.unwrap(); let result = record_continuation_delivered(&reference, "signature", None, false).await.unwrap().unwrap(); assert_eq!(result.consecutive_continuations, Some(1)); assert_eq!(result.unattended_continuations, Some(0)); }
    #[tokio::test] async fn status_update_resets_continuation_counters() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); create_goal(&reference, "Work", None, 1).await.unwrap(); record_continuation_delivered(&reference, "signature", None, true).await.unwrap(); let result = update_goal(&reference, &GoalUpdate { status: Some(GoalStatus::Paused), ..Default::default() }, GoalUpdateSource::User, 2).await.unwrap(); assert_eq!(result.consecutive_continuations, Some(0)); assert_eq!(result.unattended_continuations, Some(0)); assert_eq!(result.last_continuation_signature, None); }
    #[tokio::test] async fn clear_reports_previous_goal_presence() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); create_goal(&reference, "Work", None, 1).await.unwrap(); let result = clear_goal(&reference).await.unwrap(); assert!(result); assert!(read_goal(&reference).unwrap().is_none()); }
    #[tokio::test] async fn concurrent_creates_serialize_and_preserve_one_identity() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); let (a, b) = tokio::join!(create_goal(&reference, "a", None, 1), create_goal(&reference, "b", None, 1)); assert_ne!(a.is_ok(), b.is_ok()); assert!(read_goal(&reference).unwrap().is_some()); }
    #[tokio::test] async fn reset_preserves_unattended_count_unless_requested() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); create_goal(&reference, "Work", None, 1).await.unwrap(); record_continuation_delivered(&reference, "sig", None, true).await.unwrap(); let result = reset_continuation_streak(&reference, false).await.unwrap().unwrap(); assert_eq!(result.consecutive_continuations, Some(0)); assert_eq!(result.unattended_continuations, Some(1)); assert_eq!(result.last_continuation_signature, None); }
    #[tokio::test] async fn truncated_objective_saves_full_text_sidecar() { let dir = tempfile::tempdir().unwrap(); let reference = reference(&dir); let objective = "x".repeat(4001); let result = create_goal(&reference, &objective, None, 1).await.unwrap(); assert_eq!(result.objective.chars().count(), 4000); assert_eq!(fs::read_to_string(objective_full_text_file_path(&reference)).unwrap(), objective); }
}
