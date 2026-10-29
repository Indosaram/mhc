use super::{run_finalization_types::ReservationRunResult, runner_types::ReflectionRunResult};

pub fn require_finalized_result(result: Option<ReservationRunResult>) -> Result<ReflectionRunResult, String> {
    let result = result.ok_or_else(|| "Reflection finalization claim is busy".to_owned())?;
    if result.outcome == "abandoned_unknown" || result.completion.is_none() {
        return Err(format!("Reflection finalization did not publish completion for {}", result.run_id));
    }
    let completion = result.completion.ok_or_else(|| format!("Reflection finalization did not publish completion for {}", result.run_id))?;
    Ok(ReflectionRunResult { run_id: result.run_id, outcome: result.outcome, reason: result.reason, detail: result.detail, completion, launch: result.launch })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn busy_is_rejected() { assert_eq!(require_finalized_result(None).err().as_deref(), Some("Reflection finalization claim is busy")); }
    #[test] fn completed_result_preserves_completion_and_details() {
        let completion = serde_json::from_value(serde_json::json!({"schemaVersion":1,"runId":"run","identity":"agent","category":"quick","conversationIds":["conversation"],"trigger":"manual","outcome":"merged","startedAt":"start","finishedAt":"end","delivery":{"status":"pending"}})).unwrap();
        let result = require_finalized_result(Some(ReservationRunResult { run_id:"run".into(), outcome:"merged".into(), reason:Some("reason".into()), detail:Some("detail".into()), completion:Some(completion), launch:None })).unwrap();
        assert_eq!(result.run_id, "run"); assert_eq!(result.completion.conversation_ids, ["conversation"]); assert_eq!(result.reason.as_deref(), Some("reason")); assert_eq!(result.detail.as_deref(), Some("detail"));
    }
    #[test] fn missing_completion_and_abandonment_are_rejected() { for outcome in ["merged", "abandoned_unknown"] { let result = ReservationRunResult { run_id: "run".into(), outcome: outcome.into(), reason: None, detail: None, completion: None, launch: None }; assert!(require_finalized_result(Some(result)).is_err()); } }
}
