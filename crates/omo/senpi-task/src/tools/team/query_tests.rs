//! `tools/team/query.test.ts` (`team_status`, `team_list`).

use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::types::{MemberStatus, RuntimeBounds};

use crate::tools::team::index::build_lead_team_tools;
use crate::tools::team::query::{create_team_list_tool, create_team_status_tool};
use crate::tools::team::team_tool_fakes::{FakeTeamServiceOverrides, create_fake_team_service};
use crate::tools::team::types::{
    ActiveTeamScope, ActiveTeamSummary, DiscoveredTeamSpec, LeadTeamToolDeps, TeamStatus,
    TeamStatusConcurrency, TeamStatusMember, TeamStatusTasks, TeamToolDeps,
};

fn deps(overrides: FakeTeamServiceOverrides) -> TeamToolDeps {
    TeamToolDeps {
        service: Arc::new(create_fake_team_service(overrides)),
    }
}

fn aggregated_status() -> TeamStatus {
    TeamStatus {
        team_name: "team-alpha".to_string(),
        team_run_id: "team-run-1".to_string(),
        status: "active".to_string(),
        lead_session_id: None,
        created_at: 1,
        members: vec![TeamStatusMember {
            name: "worker".to_string(),
            session_id: None,
            status: MemberStatus::Running,
            color: None,
            worktree_path: None,
            unread_messages: 0,
            pane_id: None,
        }],
        tasks: TeamStatusTasks::default(),
        shutdown_requests: Vec::new(),
        concurrency: TeamStatusConcurrency::default(),
        bounds: RuntimeBounds::default(),
        stale_locks: Vec::new(),
    }
}

fn active_summary(name: &str, scope: ActiveTeamScope, member_count: usize) -> ActiveTeamSummary {
    ActiveTeamSummary {
        team_run_id: "run-1".to_string(),
        team_name: name.to_string(),
        status: "active".to_string(),
        member_count,
        scope,
        lead_session_id: None,
    }
}

#[test]
fn team_status_returns_aggregated_team_status() {
    // given
    let expected = aggregated_status();
    let expected_clone = expected.clone();
    let tool = create_team_status_tool(&deps(FakeTeamServiceOverrides {
        aggregate_status: Some(Box::new(move |team_run_id| {
            assert_eq!(team_run_id, "team-run-1");
            Ok(expected_clone.clone())
        })),
        ..Default::default()
    }));

    // when
    let result = tool
        .execute_json("call", &json!({ "teamRunId": "team-run-1" }))
        .expect("execute");

    // then
    assert_eq!(result["details"], serde_json::to_value(&expected).expect("status json"));
}

#[test]
fn team_list_includes_declared_only_teams() {
    // given
    let tool = create_team_list_tool(&deps(FakeTeamServiceOverrides {
        discover_team_specs: Some(Box::new(|_project_root: &Path| {
            Ok(vec![DiscoveredTeamSpec {
                name: "foo".to_string(),
                scope: ActiveTeamScope::Project,
                path: "/tmp/project/foo/config.json".to_string(),
            }])
        })),
        load_team_spec_member_count: Some(Box::new(|name, _project_root| {
            assert_eq!(name, "foo");
            Ok(1)
        })),
        list_teams: Some(Box::new(|| {
            Ok(vec![active_summary("bar", ActiveTeamScope::User, 3)])
        })),
        ..Default::default()
    }));

    // when
    let result = tool.execute_json("call", &json!({})).expect("execute");

    // then
    assert_eq!(
        result["details"],
        json!([
            { "name": "foo", "scope": "project", "status": "not-started", "memberCount": 1 },
            { "name": "bar", "scope": "user", "status": "active", "teamRunId": "run-1", "memberCount": 3 }
        ])
    );
}

#[test]
fn team_list_scope_filters_the_declared_specs() {
    // given a declared project spec and a declared user spec, with no active run.
    let tool = create_team_list_tool(&deps(FakeTeamServiceOverrides {
        discover_team_specs: Some(Box::new(|_project_root: &Path| {
            Ok(vec![
                DiscoveredTeamSpec {
                    name: "foo".to_string(),
                    scope: ActiveTeamScope::Project,
                    path: "/tmp/project/foo/config.json".to_string(),
                },
                DiscoveredTeamSpec {
                    name: "bar".to_string(),
                    scope: ActiveTeamScope::User,
                    path: "/tmp/user/bar/config.json".to_string(),
                },
            ])
        })),
        load_team_spec_member_count: Some(Box::new(|_name, _project_root| Ok(2))),
        list_teams: Some(Box::new(|| Ok(Vec::new()))),
        ..Default::default()
    }));

    // when
    let result = tool
        .execute_json("call", &json!({ "scope": "user" }))
        .expect("execute");

    // then only the user-scoped declared spec survives the filter.
    assert_eq!(
        result["details"],
        json!([{ "name": "bar", "scope": "user", "status": "not-started", "memberCount": 2 }])
    );
}

#[test]
fn build_lead_team_tools_publishes_the_query_tools() {
    // given / when
    let names: Vec<String> = build_lead_team_tools(&LeadTeamToolDeps {
        service: Arc::new(create_fake_team_service(FakeTeamServiceOverrides::default())),
    })
    .iter()
    .map(|tool| tool.name().to_string())
    .collect();

    // then
    assert!(names.contains(&"team_status".to_string()));
    assert!(names.contains(&"team_list".to_string()));
}
