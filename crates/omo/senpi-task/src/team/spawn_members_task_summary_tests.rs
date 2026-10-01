//! `team/spawn-members-task-summary.test.ts`

use std::sync::Mutex;

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::team::member_projection::ResidentSessionRef;
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::runtime_fakes::task_status;
use crate::team::runtime_types::{
    TeamCancelOutcome, TeamMemberCancelPort, TeamMemberReadPort, TeamMemberStartSpec, TeamMemberTaskRecord,
    TeamRuntimeManagerPort, TeamStartResult, TeamStartedMember,
};
use crate::team::spawn_members::{SpawnMembersInput, spawn_team_members};

struct CapturingManager {
    captured: Mutex<Vec<TeamMemberStartSpec>>,
}

impl CapturingManager {
    fn new() -> Self {
        Self {
            captured: Mutex::new(Vec::new()),
        }
    }

    fn captured(&self) -> Vec<TeamMemberStartSpec> {
        self.captured.lock().expect("captured lock").clone()
    }
}

impl TeamMemberReadPort for CapturingManager {
    fn get(&self, _task_id: &str) -> Option<TeamMemberTaskRecord> {
        None
    }
}

impl TeamMemberCancelPort for CapturingManager {
    fn cancel_task(&self, _id_or_name: &str, _reason: Option<&str>) -> TeamCancelOutcome {
        panic!("fake TeamRuntimeManagerPort.cancelTask not configured")
    }
}

impl TeamRuntimeManagerPort for CapturingManager {
    fn start(&self, spec: &TeamMemberStartSpec) -> Result<TeamStartResult, String> {
        let mut captured = self.captured.lock().expect("captured lock");
        captured.push(spec.clone());
        Ok(TeamStartResult::Started(TeamStartedMember {
            task_id: format!("st_{}", captured.len()),
            status: task_status("running"),
            name: spec.name.clone().unwrap_or_else(|| "member".to_string()),
            resolved_model: None,
        }))
    }

    fn get_resident_handle(&self, _task_id: &str) -> Option<ResidentSessionRef> {
        None
    }
}

#[test]
fn given_a_member_with_a_task_summary_when_spawned_then_the_manager_start_spec_carries_the_summary() {
    // given
    let spec = normalize_senpi_team_spec(
        &json!({ "members": [{ "kind": "category", "category": "quick", "prompt": "work", "task_summary": "Investigate the failing test" }] }),
        "demo",
        None,
    )
    .expect("spec normalizes");
    let manager = CapturingManager::new();
    let now = || chrono::Utc::now().timestamp_millis();

    // when
    let result = spawn_team_members(&SpawnMembersInput {
        spec: &spec,
        team_run_id: "run-1",
        manager: &manager,
        lead_session_id: "lead-session",
        spawn_depth: 1,
        max_parallel: 1,
        deadline_at: now() + 60_000,
        now: &now,
        member_extension: None,
    });

    // then
    assert!(result.failure.is_none());
    let captured = manager.captured();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].category.as_deref(), Some("quick"));
    assert!(format!("{:?}", spec.members[0]).contains("Investigate the failing test"));
    assert!(result.get(&spec.members[0].name).is_some());
}
