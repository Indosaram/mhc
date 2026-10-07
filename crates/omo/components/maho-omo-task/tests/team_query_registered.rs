//! Registered-driver QA for the read-only team query tools (`team_status` / `team_list`).
//!
//! The plain `mhc -p --offline --no-tools '<team_list {}>'` form is NOT a registered tool
//! invocation: it disables tools and sends a model prompt. This driver exercises the REAL
//! registered path instead - `build_lead_team_tools` -> `LeadTeamTool::execute_json` - over the REAL
//! `TeamService` (not a fake), on injected temporary project/state roots with a fixed clock. No
//! process HOME or cwd is mutated. Deterministic: no model, no
//! network, no sleeps; the `tempfile::TempDir` root drops with the fixture (no process to tear down).
//!
//! Run: `cargo nextest run -p maho-omo-task --test team_query_registered`.
//!
//! Dependency: this file compiles once the runtime lane's RU-05 producer source
//! (`crates/omo/senpi-task/src/tools/team/{query,index,types}.rs`, `maho-omo-task/src/team_service.rs`
//! `aggregate_status`/`discover_team_specs`) is integrated into this tree; it is authored against that
//! exact API before integration, per the authoring-before-checks rule.

use std::{collections::BTreeSet, sync::{Arc, Mutex}};
use maho_omo_task::team_service::{TeamService, TeamServiceDeps, create_team_service};
use senpi_task::{
    manager::{create_task_manager, types::{ManagedRunner, ManagedRunnerResult, ManagedStartSpec, ManagedRunners, TaskManagerOptions, ResolvedChildPlan}},
    store::{StateDirConfig, TaskRecordStore},
    team::{runtime_types::*, runtime_config::{TeamTaskBounds, to_team_core_config}, storage::team_storage_base_dir, normalize::normalize_senpi_team_spec, member_projection::ResidentSessionRef},
    lifecycle::DestroyCause,
    tools::team::{index::{build_lead_team_tools, LeadTeamTool}, types::{LeadTeamToolDeps, TeamToolsService}},
};
use team_core::{team_state_store::{create_runtime_state, transition_runtime_state}, types::{SpecSource, RuntimeStatus}};
use serde_json::json;

struct NoLaunch;
impl ManagedRunner for NoLaunch { fn start(&self, _: &ManagedStartSpec) -> ManagedRunnerResult { panic!("unexpected child launch") } }
struct Members;
impl TeamMemberReadPort for Members { fn get(&self, _: &str) -> Option<TeamMemberTaskRecord> { None } }
impl TeamMemberCancelPort for Members { fn cancel_task(&self, _: &str, _: Option<&str>) -> TeamCancelOutcome { panic!("unexpected cancellation") } }
impl TeamRuntimeManagerPort for Members { fn start(&self, _: &TeamMemberStartSpec) -> Result<TeamStartResult, String> { panic!("unexpected member launch") } fn get_resident_handle(&self, _: &str) -> Option<ResidentSessionRef> { None } }
impl TeamMemberDestructionPort for Members { fn destroy_resident_task(&self, _: &str, _: DestroyCause) -> Result<(), String> { panic!("unexpected destruction") } }

struct Fixture { service: Arc<TeamService>, run: String, _root: tempfile::TempDir }

/// The real service over private project/state roots, active and declared specs, and a fixed clock.
fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("team root");
    let state_dir = StateDirConfig { project_dir: root.path().into(), task_state_dir: None };
    let bounds = TeamTaskBounds { max_members: 4, max_parallel_members: 2, max_wall_clock_minutes: 10 };
    let manager = create_task_manager(TaskManagerOptions::new(
        TaskRecordStore::new(&state_dir),
        ManagedRunners { in_process: Arc::new(NoLaunch), process: Arc::new(NoLaunch) },
        Arc::new(|_| Ok(ResolvedChildPlan { model: "faux/faux".into(), ..Default::default() })),
        root.path().to_string_lossy(),
    ));
    let config = to_team_core_config(&bounds, &team_storage_base_dir(&state_dir).to_string_lossy()).expect("config");
    // Declared specs are validated by team-core on load: a category member needs a prompt of at
    // least 8 characters, and a multi-member spec must name its lead (leadAgentId = a member name).
    let declared = root.path().join(".omo/teams/declared-only");
    std::fs::create_dir_all(&declared).expect("declared spec directory");
    std::fs::write(declared.join("config.json"), json!({"name": "declared-only", "members": [{"name": "declared", "kind": "category", "category": "quick", "prompt": "implement the assigned task"}]}).to_string()).expect("declared spec");
    let project_teams = root.path().join(".omo/teams");
    let user_teams = team_storage_base_dir(&state_dir).join("teams");
    for (directory, name, count) in [
        (&project_teams, "shadowed", 3usize),
        (&user_teams, "shadowed", 1usize),
        (&user_teams, "user-only", 2usize),
    ] {
        let path = directory.join(name);
        std::fs::create_dir_all(&path).expect("spec directory");
        let members = (0..count).map(|index| json!({"name": format!("m{index}"), "kind": "category", "category": "quick", "prompt": "implement the assigned task"})).collect::<Vec<_>>();
        std::fs::write(path.join("config.json"), json!({"name": name, "leadAgentId": "m0", "members": members}).to_string()).expect("spec file");
    }
    let spec = normalize_senpi_team_spec(&json!({"members": [{"name": "beta", "kind": "category", "category": "quick", "prompt": "work"}]}), "squad", None).expect("spec");
    let state = create_runtime_state(&spec, Some("lead"), SpecSource::Project, &config).expect("runtime");
    let run = state.team_run_id.clone();
    transition_runtime_state(&run, |mut state| { state.status = RuntimeStatus::Active; state }, &config).expect("active");
    let session = Arc::new(Mutex::new(Some("lead".to_owned())));
    let id = session.clone();
    let service = create_team_service(TeamServiceDeps {
        manager: Arc::new(manager),
        member_manager: Arc::new(Members),
        destruction: Arc::new(Members),
        session_id: Arc::new(move || id.lock().expect("session").clone()),
        state_dir: state_dir.clone(),
        bounds,
        omo_config: json!({}),
        agent_names: BTreeSet::new(),
        member_extension: TeamMemberExtensionConfig::default(),
        append_task_event: None,
        now: Some(Arc::new(|| 1000)),
        new_message_id: Some(Arc::new(|| "77777777-7777-4777-8777-777777777777".into())),
    })
    .expect("service");
    Fixture { service: Arc::new(service), run, _root: root }
}

fn tools(fixture: &Fixture) -> Vec<LeadTeamTool> {
    let service: Arc<dyn TeamToolsService> = fixture.service.clone();
    build_lead_team_tools(&LeadTeamToolDeps { service })
}

fn named<'a>(tools: &'a [LeadTeamTool], name: &str) -> &'a LeadTeamTool {
    tools.iter().find(|tool| tool.name() == name).unwrap_or_else(|| panic!("registered tool {name} is absent"))
}

/// T1: the registrar publishes both query tools alongside the six mutating ones.
#[test]
fn registrar_publishes_the_two_read_only_query_tools() {
    let f = fixture();
    let registered = tools(&f);
    let names: Vec<&str> = registered.iter().map(LeadTeamTool::name).collect();
    assert_eq!(names, ["team_create", "team_delete", "task_create", "task_get", "task_list", "task_update", "team_status", "team_list"]);
}

/// T2: `team_list` joins the active run with the declared-only spec (`not-started`, no run id).
#[test]
fn team_list_reports_active_and_not_started_entries() {
    let f = fixture();
    let value = named(&tools(&f), "team_list").execute_json("call-list", &json!({})).expect("team_list");
    let entries = value["details"].as_array().expect("team_list details array");
    assert!(entries.iter().any(|entry| entry["status"] == "active"), "active run missing: {value}");
    let not_started = entries.iter().find(|entry| entry["status"] == "not-started").expect("declared-only entry");
    assert!(not_started.get("teamRunId").is_none(), "a not-started entry carries no run id: {not_started}");
}

/// T3: `team_status` projects the run through `aggregateStatus`.
#[test]
fn team_status_projects_the_run_aggregate() {
    let f = fixture();
    let value = named(&tools(&f), "team_status").execute_json("call-status", &json!({ "teamRunId": f.run })).expect("team_status");
    let details = &value["details"];
    assert_eq!(details["teamRunId"], json!(f.run));
    assert_eq!(details["status"], json!("active"));
    for key in ["teamName", "createdAt", "members", "tasks", "concurrency"] {
        assert!(details.get(key).is_some(), "aggregate missing {key}: {value}");
    }
}

/// T4: an unknown (but canonical) run id is a named error, never a panic or an empty success.
#[test]
fn team_status_rejects_an_unknown_run() {
    let f = fixture();
    let unknown = "11111111-1111-4111-8111-111111111111";
    let error = named(&tools(&f), "team_status").execute_json("call-missing", &json!({ "teamRunId": unknown })).expect_err("unknown run");
    assert!(!error.to_string().is_empty(), "the not-found error names the run");
}

#[test]
fn team_list_prefers_the_project_spec_over_the_same_named_user_spec() {
    let f = fixture();
    let value = named(&tools(&f), "team_list").execute_json("precedence", &json!({})).expect("team_list");
    let entries = value["details"].as_array().expect("entries");
    let shadowed = entries.iter().filter(|entry| entry["name"] == "shadowed").collect::<Vec<_>>();
    assert_eq!(shadowed.len(), 1);
    assert_eq!(shadowed[0]["scope"], json!("project"));
    assert_eq!(shadowed[0]["memberCount"], json!(3));
}

#[test]
fn team_list_filters_nonempty_project_and_user_scopes() {
    let f = fixture();
    for (scope, expected_name, expected_count) in [("user", "user-only", 2), ("project", "shadowed", 3)] {
        let value = named(&tools(&f), "team_list").execute_json("scope", &json!({"scope": scope})).expect("team_list");
        let entries = value["details"].as_array().expect("entries");
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|entry| entry["scope"] == scope));
        let entry = entries.iter().find(|entry| entry["name"] == expected_name).expect("scope-specific spec");
        assert_eq!(entry["memberCount"], json!(expected_count));
    }
}
