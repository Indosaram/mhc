//! `tools/team/allowlist.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::runners::in_process::shared_tool_filter::is_task_or_team_family_tool;
use crate::tools::control::send::{MemberScopedTaskSendDeps, create_member_scoped_task_send_tool};
use crate::tools::control::types::{
    ControlListScope, ControlSendInput, ControlTaskRecord, SendManager, SendOutcome,
};
use crate::tools::team::index::{LeadTeamTool, build_lead_team_tools};
use crate::tools::team::team_tool_fakes::{FakeTeamServiceOverrides, create_fake_team_service};
use crate::tools::team::types::{LeadTeamToolDeps, TeamToolsService};

struct FakeSendManager;

impl SendManager for FakeSendManager {
    fn send_to_task(&self, _input: &ControlSendInput) -> Result<SendOutcome, String> {
        Ok(SendOutcome::NotFound {
            reason: "missing".to_string(),
        })
    }

    fn list(&self, _scope: &ControlListScope) -> Vec<ControlTaskRecord> {
        Vec::new()
    }
}

fn fake_service() -> Arc<dyn TeamToolsService> {
    Arc::new(create_fake_team_service(FakeTeamServiceOverrides::default()))
}

fn lead_tool_deps() -> LeadTeamToolDeps {
    LeadTeamToolDeps {
        service: fake_service(),
    }
}

fn tool_names(tools: &[LeadTeamTool]) -> Vec<String> {
    tools.iter().map(|tool| tool.name().to_string()).collect()
}

/// Mirrors `filterSharedParentTools`: task/team family tools never pass through to a child.
fn filter_shared_names(names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| !is_task_or_team_family_tool(name))
        .cloned()
        .collect()
}

#[test]
fn given_the_lead_team_tools_w2lead_when_built_then_the_injection_only_surface_has_no_blocking_wait() {
    // given / when
    let tools = build_lead_team_tools(&lead_tool_deps());

    // then
    assert_eq!(
        tool_names(&tools),
        vec![
            "team_create".to_string(),
            "team_delete".to_string(),
            "task_create".to_string(),
            "task_get".to_string(),
            "task_list".to_string(),
            "task_update".to_string(),
        ]
    );
}

#[test]
fn given_the_lead_team_tools_as_shared_parent_tools_when_filtered_for_a_child_then_all_are_excluded() {
    // given
    let team_tools = build_lead_team_tools(&lead_tool_deps());

    // when
    let child_tools = filter_shared_names(&tool_names(&team_tools));

    // then
    assert_eq!(child_tools.len(), 0);
}

#[test]
fn given_a_member_with_the_pre_scoped_send_when_child_tools_merge_then_only_task_send_survives() {
    let team_tools = build_lead_team_tools(&lead_tool_deps());
    let member_send = create_member_scoped_task_send_tool(MemberScopedTaskSendDeps {
        manager: Arc::new(FakeSendManager),
        service: fake_service(),
        team_run_id: "run-1".to_string(),
        from: "alpha".to_string(),
        resolve_caller_session_id: None,
    });

    // when: shared parent tools are filtered, then the member's custom tools are appended.
    let mut child_tools = filter_shared_names(&tool_names(&team_tools));
    child_tools.push(member_send.name.to_string());

    // then
    assert_eq!(child_tools, vec!["task_send".to_string()]);
}
