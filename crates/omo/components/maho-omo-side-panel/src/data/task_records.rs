use serde::Deserialize;
use senpi_task::state::recovery_presentation::{RecoveryRecord, RecoveryState, recovery_presentation};
use crate::store::{PanelChildStatus,PanelChildUpdate};

#[derive(Deserialize)]
pub struct PanelTaskRecord {
    pub task_id:String,pub status:String,pub created_at:String,
    pub started_at:Option<String>,pub terminal_at:Option<String>,pub parent_session_id:Option<String>,
    pub task_summary:Option<String>,pub description:Option<String>,pub name:Option<String>,pub category:Option<String>,
    pub agent_type:Option<String>,pub failure_kind:Option<String>,pub error_message:Option<String>,
    pub suspension_reason:Option<String>,pub revival_deferred_reason:Option<String>,pub residency_state:Option<String>,
    pub runner_kind:Option<String>,pub execution_mode:Option<String>,pub run_stats:Option<PanelTaskRunStats>,
}
#[derive(Deserialize)]
pub struct PanelTaskRunStats {pub turns:Option<u64>,pub total_tokens:Option<u64>,pub cost_usd:Option<f64>}

pub fn panel_child_from_record(record:&PanelTaskRecord)->PanelChildUpdate {
    let mut status=match record.status.as_str() {
        "running"=>PanelChildStatus::Running,"completed"=>PanelChildStatus::Finished,
        "error"|"lost"=>PanelChildStatus::Failed,"cancelled"|"interrupted"=>PanelChildStatus::Cancelled,
        _=>PanelChildStatus::Queued,
    };
    let recovery = recovery_presentation(&RecoveryRecord {
        status: &record.status, residency_state: record.residency_state.as_deref(),
        suspension_reason: record.suspension_reason.as_deref(),
        revival_deferred_reason: record.revival_deferred_reason.as_deref(),
        failure_kind: record.failure_kind.as_deref(), error_message: record.error_message.as_deref(),
    });
    let parked_reason = recovery.map(|recovery| {
        status = match recovery.state { RecoveryState::Ending => PanelChildStatus::Ending, RecoveryState::Resuming => PanelChildStatus::Resuming };
        recovery.cause
    });
    let name=[&record.task_summary,&record.description,&record.name,&record.agent_type].into_iter()
        .filter_map(|value|value.as_deref()).map(str::trim).find(|value|!value.is_empty()).unwrap_or(&record.task_id).to_owned();
    let timestamp=|value:Option<&str>|value.and_then(|value|chrono::DateTime::parse_from_rfc3339(value).ok())
        .and_then(|value|u64::try_from(value.timestamp_millis()).ok());
    PanelChildUpdate {id:record.task_id.clone(),name:Some(name),category:record.category.clone(),status:Some(status),parked_reason,
        host:match record.runner_kind.as_deref() {Some("host-session")=>Some("daemon session".into()),Some("child-process")=>Some("child process".into()),_=>record.execution_mode.clone()},
        started_at:Some(timestamp(record.started_at.as_deref()).or_else(||timestamp(Some(&record.created_at))).unwrap_or(0)),
        finished_at:timestamp(record.terminal_at.as_deref()),turns:record.run_stats.as_ref().and_then(|stats|stats.turns),
        tokens:record.run_stats.as_ref().and_then(|stats|stats.total_tokens),cost:record.run_stats.as_ref().and_then(|stats|stats.cost_usd),..Default::default()}
}

pub fn panel_children_from_records(records:&[PanelTaskRecord],session:Option<&str>)->Vec<PanelChildUpdate> {
    let Some(session)=session.filter(|session|!session.is_empty()) else {return Vec::new();};
    records.iter().filter(|record|record.parent_session_id.as_deref()==Some(session)).map(panel_child_from_record).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_children_are_parked_and_scoped_to_their_parent() {
        let mut record:PanelTaskRecord=serde_json::from_value(serde_json::json!({
            "task_id":"child","status":"running","created_at":"2026-10-20T00:00:00Z",
            "parent_session_id":"parent","residency_state":"persisted_only","runner_kind":"host-session"
        })).expect("record");
        let child=panel_child_from_record(&record);
        assert_eq!(child.status,Some(PanelChildStatus::Resuming));
        assert_eq!(child.parked_reason.as_deref(),Some("parent session restarted"));
        assert_eq!(child.host.as_deref(),Some("daemon session"));
        record.suspension_reason=Some("host_incompatible".into());
        assert_eq!(panel_child_from_record(&record).status,Some(PanelChildStatus::Ending));
        record.status="completed".into();
        assert_eq!(panel_child_from_record(&record).status,Some(PanelChildStatus::Finished));
        assert!(panel_child_from_record(&record).parked_reason.is_none());
        assert!(panel_children_from_records(std::slice::from_ref(&record),None).is_empty());
        assert!(panel_children_from_records(std::slice::from_ref(&record),Some("other")).is_empty());
        assert_eq!(panel_children_from_records(&[record],Some("parent")).len(),1);
    }
}
