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
    assert_eq!(captured[0].task_summary.as_deref(), Some("Investigate the failing test"));
    assert!(captured[0].run_in_background);
    assert!(result.get(&spec.members[0].name).is_some());
}

#[test]
fn member_launch_extras_survive_the_manager_adapter() {
    let root = tempfile::tempdir().expect("worktree root");
    let cwd = root.path().join("member").to_string_lossy().into_owned();
    let spec = normalize_senpi_team_spec(&json!({"members":[{"name":"worker","kind":"category","category":"quick","worktreePath":cwd,"task_summary":"Repair parser"}]}), "demo", None).expect("spec");
    let manager = CapturingManager::new();
    let result = spawn_team_members(&SpawnMembersInput {
        spec:&spec, team_run_id:"run-1", manager:&manager, lead_session_id:"lead", spawn_depth:1,
        max_parallel:1, deadline_at:10, now:&|| 1,
        member_extension:Some(crate::team::runtime_types::SpawnMemberExtensionConfig {
            entry_path:"member-extension".into(), inherited_extensions:Some(vec!["inherited".into(),"member-extension".into()]), team_config:"{\"team\":1}".into(),
        }),
    });
    assert!(result.failure.is_none());
    let captured = manager.captured();
    let launch = crate::manager::types::ManagerStartSpec::from(&captured[0]);
    assert_eq!(launch.task_summary.as_deref(), Some("Repair parser"));
    assert_eq!(launch.cwd.as_deref(), Some(cwd.as_str()));
    assert_eq!(launch.extensions, Some(vec!["member-extension".into(),"inherited".into()]));
    assert_eq!(launch.member_env, Some([("SENPI_TASK_MEMBER".into(),"run-1::worker".into()),("SENPI_TASK_TEAM_CONFIG".into(),"{\"team\":1}".into())].into()));
    assert!(launch.run_in_background);
    assert_eq!(launch.root_session_id.as_deref(), Some("lead"));
}
