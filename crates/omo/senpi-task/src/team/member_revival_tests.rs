//! `team/member-revival.test.ts`
//!
//! The TS suite drives the full respawn -> switch -> reattach chain through a fake rpc respawn
//! runner wired into `createTaskManager`. The Rust manager's rpc respawn seam is not part of the
//! public surface available to this slice, so the revival cases exercise the same decision point
//! directly: the trusted `TeamMemberRespawnLaunchResolver` that the manager consults before a
//! respawn. The reattach-disabled case runs through the real lifecycle reconcile.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use team_core::team_state_store::save_runtime_state;
use team_core::types::{RuntimeState, RuntimeStatus};

use crate::lifecycle::port::{ResidencyRegistry, ResidentHandle};
use crate::lifecycle::{LifecycleDeps, TaskSettings, create_task_lifecycle};
use crate::runners::rpc::spawn::resolve_child_session_dir;
use crate::state::{ResidencyState, TaskStatus};
use crate::store::{StateDirConfig, TaskRecordStore};
use crate::team::member_map::{MemberTaskMap, write_member_task_map};
use crate::team::member_respawn::{
    TeamMemberRespawnLaunchErrorCode, TeamMemberRespawnLaunchResolver,
    TeamMemberRespawnLaunchResolverOptions, create_team_member_respawn_launch_resolver,
};
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::registry::TeamSpecSource;
use crate::team::runtime::create_team;
use crate::team::runtime_config::{TeamTaskBounds, to_team_core_config};
use crate::team::runtime_fakes::{
    FakeTeamManager, FakeTeamManagerOptions, TeamBoundsOverrides, team_bounds, temp_project_dir,
};
use crate::team::runtime_types::{CreateTeamDeps, TeamMemberExtensionConfig};
use crate::team::storage::{resolve_team_runtime_dirs, team_storage_base_dir};
use crate::test_support::base_record;

const LEAD_SESSION_ID: &str = "lead-session";
const TRUSTED_ENTRY: &str = "/trusted/member-extension.js";
const TRUSTED_INHERITED: &str = "/trusted/provider-extension.js";
// The record store requires st_[0-9a-f]{8} ids; the member sidecar is re-pointed at this one.
const TASK_ID: &str = "st_00000001";
// Above any kernel pid_max, so the default liveness probe can never find (or signal) a real process.
const DEAD_PID: i64 = 99_999_999;

struct EmptyRegistry;

impl ResidencyRegistry for EmptyRegistry {
    fn get(&self, _task_id: &str) -> Option<Arc<dyn ResidentHandle>> {
        None
    }

    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>> {
        Vec::new()
    }

    fn forget(&self, _task_id: &str) {}

    fn has_pending_sends(&self, _task_id: &str) -> bool {
        false
    }
}

fn state_dir_config(project_dir: &Path) -> StateDirConfig {
    StateDirConfig {
        project_dir: project_dir.to_path_buf(),
        task_state_dir: None,
    }
}

fn member_extension() -> TeamMemberExtensionConfig {
    TeamMemberExtensionConfig {
        entry_path: TRUSTED_ENTRY.to_string(),
        inherited_extensions: Some(vec![TRUSTED_INHERITED.to_string()]),
    }
}

fn settings(overrides: Value) -> TaskSettings {
    TaskSettings::resolve(&overrides).expect("valid task settings")
}

fn read_events(store: &TaskRecordStore, task_id: &str) -> Vec<String> {
    let path = store.state_dir().join("logs").join(format!("{task_id}.jsonl"));
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter(|line| !line.is_empty())
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .filter_map(|event| event.get("type").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn persist_session(store: &TaskRecordStore, task_id: &str) -> PathBuf {
    let children = store.state_dir().join("children").join(task_id);
    let directory = PathBuf::from(resolve_child_session_dir(&children.to_string_lossy(), task_id));
    std::fs::create_dir_all(&directory).expect("create session dir");
    let session_path = directory.join("2026-07-12_session.jsonl");
    std::fs::write(&session_path, "{}\n").expect("write session");
    let file = std::fs::File::options()
        .write(true)
        .open(&session_path)
        .expect("open session");
    file.set_modified(UNIX_EPOCH + Duration::from_millis(2_000))
        .expect("set session mtime");
    session_path
}

struct Harness {
    project_dir: PathBuf,
    bounds: TeamTaskBounds,
    runtime_state: RuntimeState,
    store: Arc<TaskRecordStore>,
    session_path: PathBuf,
    _store_dir: tempfile::TempDir,
}

impl Harness {
    fn state_dir(&self) -> StateDirConfig {
        state_dir_config(&self.project_dir)
    }

    fn team_run_id(&self) -> &str {
        &self.runtime_state.team_run_id
    }

    fn resolver(&self) -> TeamMemberRespawnLaunchResolver {
        create_team_member_respawn_launch_resolver(TeamMemberRespawnLaunchResolverOptions {
            state_dir: self.state_dir(),
            team_bounds: self.bounds,
            member_extension: member_extension(),
        })
        .expect("resolver")
    }

    fn record_name(&self) -> Option<String> {
        self.store
            .load(TASK_ID)
            .expect("load record")
            .expect("record present")
            .name
    }

    fn assert_still_suspended(&self) {
        let record = self.store.load(TASK_ID).expect("load").expect("record present");
        assert_eq!(record.status.as_str(), "running");
        assert_eq!(record.residency_state.as_str(), "rpc_detached");
        assert_eq!(record.host_pid, None);
        assert!(!read_events(&self.store, TASK_ID).contains(&"reconcile_lost".to_string()));
    }
}

fn given_team_with_suspended_member() -> Harness {
    let project_dir = temp_project_dir();
    let bounds = team_bounds(TeamBoundsOverrides::default());
    let team_manager = Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default()));
    let spec = normalize_senpi_team_spec(
        &json!({ "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "task alpha" }] }),
        "squad",
        None,
    )
    .expect("valid spec");
    let created = create_team(
        &spec,
        TeamSpecSource::Project,
        &CreateTeamDeps {
            manager: team_manager,
            state_dir: state_dir_config(&project_dir),
            team_bounds: bounds,
            lead_session_id: LEAD_SESSION_ID.to_string(),
            spawn_depth: 1,
            now: None,
            member_extension: Some(member_extension()),
            write_member_map: None,
        },
    )
    .expect("team created");
    assert!(created.member_task_ids.contains_key("alpha"), "expected alpha task id");
    let team_run_id = created.runtime_state.team_run_id.clone();
    let runtime_dir = resolve_team_runtime_dirs(&state_dir_config(&project_dir), &team_run_id)
        .expect("runtime dirs")
        .runtime_dir;
    let map: MemberTaskMap = [("alpha".to_string(), TASK_ID.to_string())].into_iter().collect();
    write_member_task_map(&runtime_dir, &map).expect("write member map");

    let store_dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(TaskRecordStore::new(&state_dir_config(store_dir.path())));
    let mut record = base_record(TASK_ID, LEAD_SESSION_ID);
    record.name = Some(format!("team:{team_run_id}:alpha"));
    record.execution_mode = "process".to_string();
    record.model = "anthropic/claude".to_string();
    record.status = TaskStatus::parse("running").expect("status");
    record.residency_state = ResidencyState::parse("rpc_detached").expect("residency");
    record.pid = Some(900);
    record.host_pid = None;
    store.save(&record).expect("seed record");

    // Persisted spawn spec carrying untrusted launch inputs.
    let record_path = store.state_dir().join("tasks").join(format!("{TASK_ID}.json"));
    let raw = std::fs::read_to_string(&record_path).expect("read record");
    let mut value: Value = serde_json::from_str(&raw).expect("record json");
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "spawn_spec".to_string(),
            json!({
                "cwd": "/tmp/project",
                "extensions": ["/tmp/malicious-extension.ts"],
                "member_env": { "SENPI_TASK_MEMBER": "untrusted::member" },
            }),
        );
    }
    std::fs::write(&record_path, value.to_string()).expect("write record");
    assert!(store.load(TASK_ID).expect("load").is_some(), "expected seeded member record");

    let session_path = persist_session(&store, TASK_ID);
    Harness {
        project_dir,
        bounds,
        runtime_state: created.runtime_state,
        store,
        session_path,
        _store_dir: store_dir,
    }
}

#[test]
fn completed_resident_member_with_dead_process_is_recorded_lost_when_reattach_disabled() {
    // given
    let h = given_team_with_suspended_member();
    let mut record = h.store.load(TASK_ID).expect("load").expect("record");
    record.host_pid = None;
    record.status = TaskStatus::parse("completed").expect("status");
    record.residency_state = ResidencyState::parse("resident").expect("residency");
    record.pid = Some(DEAD_PID);
    h.store.replace(&record).expect("replace");
    let store: Arc<TaskRecordStore> = Arc::clone(&h.store);
    let lifecycle = create_task_lifecycle(LifecycleDeps::new(
        store,
        Arc::new(EmptyRegistry),
        settings(json!({ "reattach_on_reconcile": false })),
    ));

    // when
    let result = lifecycle
        .reconcile_on_session_start(Some(LEAD_SESSION_ID))
        .expect("reconcile");

    // then
    let rendered = format!("{result:?}");
    assert!(rendered.contains(TASK_ID), "{rendered}");
    assert!(rendered.contains("reattach disabled for crashed resident"), "{rendered}");
    assert!(rendered.to_lowercase().contains("lost"), "{rendered}");
    let record = h.store.load(TASK_ID).expect("load").expect("record");
    assert_eq!(record.status.as_str(), "completed");
    assert_eq!(record.residency_state.as_str(), "disposed");
    assert_eq!(record.killed, Some(true));
    assert_eq!(
        record.error_message.as_deref(),
        Some("reattach disabled for crashed resident")
    );
    assert!(read_events(&h.store, TASK_ID).contains(&"reconcile_lost".to_string()));
}

#[test]
fn suspended_team_member_revives_with_trusted_launch_inputs_and_keeps_mailbox_identity() {
    // given a team member record suspended at shutdown, whose persisted spec carries untrusted inputs
    let h = given_team_with_suspended_member();

    // when the owning session resolves the respawn launch
    let launch = h
        .resolver()
        .resolve(h.record_name().as_deref(), TASK_ID)
        .expect("trusted launch");

    // then extensions and member env come from the current team runtime, never the persisted spec
    assert_eq!(
        launch.extensions,
        vec![TRUSTED_ENTRY.to_string(), TRUSTED_INHERITED.to_string()]
    );
    let env = launch.member_env.expect("member env");
    let expected_member = format!("{}::alpha", h.team_run_id());
    assert_eq!(
        env.get("SENPI_TASK_MEMBER").map(String::as_str),
        Some(expected_member.as_str())
    );
    assert_eq!(
        env.get("SENPI_TASK_MEMBER_TASK_ID").map(String::as_str),
        Some(TASK_ID)
    );
    let team_config = env.get("SENPI_TASK_TEAM_CONFIG").expect("team config");
    assert!(!team_config.contains("untrusted"));
    assert!(h.session_path.is_file());
    let record = h.store.load(TASK_ID).expect("load").expect("record");
    assert_eq!(record.status.as_str(), "running");
}

#[test]
fn suspended_member_of_inactive_team_stays_suspended_with_team_inactive() {
    // given the team runtime transitioned out of an active state while the member was suspended
    let h = given_team_with_suspended_member();
    let mut failed = h.runtime_state.clone();
    failed.status = RuntimeStatus::Failed;
    let base_dir = team_storage_base_dir(&h.state_dir()).to_string_lossy().into_owned();
    let config = to_team_core_config(&h.bounds, &base_dir).expect("config");
    save_runtime_state(&failed, &config).expect("save runtime state");

    // when the owning session resolves the respawn launch
    let error = h
        .resolver()
        .resolve(h.record_name().as_deref(), TASK_ID)
        .expect_err("inactive team must not revive");

    // then the member is NOT revived and NOT lost
    assert_eq!(error.code, TeamMemberRespawnLaunchErrorCode::RuntimeInactive);
    assert_eq!(
        error.message,
        "Cannot respawn team member 'alpha': runtime_inactive"
    );
    h.assert_still_suspended();
}

#[test]
fn suspended_member_of_deleted_team_stays_suspended_with_team_inactive() {
    // given the team runtime dir was deleted while the member was suspended
    let h = given_team_with_suspended_member();
    let runtime_dir = resolve_team_runtime_dirs(&h.state_dir(), h.team_run_id())
        .expect("runtime dirs")
        .runtime_dir;
    std::fs::remove_dir_all(&runtime_dir).expect("remove runtime dir");

    // when the owning session resolves the respawn launch
    let error = h
        .resolver()
        .resolve(h.record_name().as_deref(), TASK_ID)
        .expect_err("deleted team must not revive");

    // then the member is NOT revived and NOT lost
    assert_eq!(error.code, TeamMemberRespawnLaunchErrorCode::RuntimeUnavailable);
    assert_eq!(
        error.message,
        "Cannot respawn team member 'alpha': runtime_unavailable"
    );
    h.assert_still_suspended();
}
