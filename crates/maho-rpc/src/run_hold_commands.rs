use maho_core::agent_session::AgentSession;
use serde_json::{Value, json};

const NOT_TASK_PARENT: &str = "task_parent_required: only the connection that owns this task child may pause or resume it for a run";

pub async fn run_pause_for_run(
    session: &AgentSession,
    command: &Value,
    task_parent: bool,
) -> Result<Value, String> {
    if !task_parent {
        return Err(NOT_TASK_PARENT.into());
    }
    let run =
        serde_json::from_value(command["runRef"].clone()).map_err(|error| error.to_string())?;
    let parent = command["parentSessionId"].as_str().unwrap_or_default();
    let parent_seq = command["parentPauseSeq"]
        .as_u64()
        .or_else(|| command["parentPauseSeq"].as_i64().map(|v| v as u64))
        .unwrap_or_default();
    let result = session.pause_for_run(parent, parent_seq, run).await?;
    match result {
        maho_core::run_holds::InheritResult::Inherited { child_seq } => {
            Ok(json!({"held":true,"childSeq":child_seq}))
        }
        maho_core::run_holds::InheritResult::WrongParent => {
            Err("wrong_parent: this child is paused for another parent session".into())
        }
    }
}

pub async fn run_resume_for_run(
    session: &AgentSession,
    command: &Value,
    task_parent: bool,
) -> Result<Value, String> {
    if !task_parent {
        return Err(NOT_TASK_PARENT.into());
    }
    let parent = command["parentSessionId"].as_str().unwrap_or_default();
    let parent_seq = command["parentPauseSeq"]
        .as_u64()
        .or_else(|| command["parentPauseSeq"].as_i64().map(|v| v as u64))
        .unwrap_or_default();
    let cutoff = command["childSeqCutoff"]
        .as_u64()
        .or_else(|| command["childSeqCutoff"].as_i64().map(|v| v as u64))
        .unwrap_or_default();
    let result = session.resume_for_run(parent, parent_seq, cutoff).await?;
    use maho_core::run_holds::ReleaseInheritedResult;
    match result {
        ReleaseInheritedResult::Released { other_hold_live } => {
            Ok(json!({"released":true,"otherHoldLive":other_hold_live}))
        }
        ReleaseInheritedResult::WrongParent => {
            Err("wrong_parent: this child is paused for another parent session".into())
        }
        ReleaseInheritedResult::Stale => Ok(json!({"released":false,"reason":"stale"})),
        ReleaseInheritedResult::NotHeld => Ok(json!({"released":false,"reason":"not_held"})),
    }
}
