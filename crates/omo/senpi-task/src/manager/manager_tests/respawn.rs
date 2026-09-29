//! `manager/manager-respawn-cleanup.test.ts`: respawn cleanup, launch trust, variant,
//! in-process resume, guarded reattach and interrupted-turn continuation.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::fakes::{FakeHandle, FakeRunner, Project, category_planner, config, lock};
use crate::host::HostError;
use crate::lifecycle::RespawnResult;
use crate::lifecycle::port::{
    ReattachFailureKind, ReattachResult, RespawnDisposition, RespawnFailureCode,
};
use crate::manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::manager::parent_registry_context::create_parent_registry_session_context;
use crate::manager::runner::{
    InProcessHandleResult, InProcessRunnerLike, create_in_process_managed_runner,
};
use crate::manager::types::{
    ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec, RpcRespawnRunner,
    TaskManagerOptions, TrustedRespawnLaunch,
};
use crate::manager::{TaskManager, create_task_manager};
use crate::runners::RunnerOutcome;
use crate::runners::in_process::ChildSpec;
use crate::runners::types::{RpcRunnerSpec, RpcSwitchSessionResult};
use crate::state::{
    ResidencyState, ResolvedModelRecord, ResolvedModelSource, SpawnSpecV1, TaskNotification,
    TaskRecord, TaskRecordInput, TaskSpawnSpec, TaskStatus, create_task_record,
};

const HOST_PID: i64 = 7001;

fn respawn_record() -> TaskRecord {
    let draft = create_task_record(
        TaskRecordInput {
            name: Some("reattach-me".into()),
            parent_session_id: "parent-1".into(),
            root_session_id: "parent-1".into(),
            depth: 1,
            execution_mode: "process".into(),
            model: "openai/gpt-5.6".into(),
            ..TaskRecordInput::default()
        },
        None,
    )
    .expect("record");
    TaskRecord {
        task_id: "st_deadbeef".to_string(),
        status: TaskStatus::Running,
        residency_state: ResidencyState::Resident,
        created_at: "2026-07-12T00:00:00.000Z".to_string(),
        updated_at: "2026-07-12T00:01:00.000Z".to_string(),
        spawn_spec: Some(legacy_spawn("/tmp/project", None, None)),
        ..draft
    }
}

fn legacy_spawn(
    cwd: &str,
    extensions: Option<Vec<String>>,
    member_env: Option<Vec<(String, String)>>,
) -> TaskSpawnSpec {
    TaskSpawnSpec::LegacyProcess {
        cwd: cwd.to_string(),
        extensions,
        member_env,
    }
}

fn agent_model(provider: &str, model_id: &str, display: &str) -> ResolvedModelRecord {
    ResolvedModelRecord {
        display: display.to_string(),
        ..ResolvedModelRecord::new(ResolvedModelSource::Agent, provider, model_id)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CleanupStage {
    None,
    Terminate,
    Dispose,
}

/// The TS `RpcChildHandle` literal: records follow-ups and dispose calls.
struct RpcFake {
    task_id: String,
    cleanup_fails: CleanupStage,
    switch_cancelled: bool,
    follow_ups: Mutex<Vec<String>>,
    dispose_calls: Mutex<usize>,
}

impl RpcFake {
    fn new(cleanup_fails: CleanupStage, switch_cancelled: bool) -> Arc<Self> {
        Arc::new(Self {
            task_id: "st_deadbeef".to_string(),
            cleanup_fails,
            switch_cancelled,
            follow_ups: Mutex::default(),
            dispose_calls: Mutex::default(),
        })
    }

    fn rejects(&self, stage: CleanupStage) -> Result<(), HostError> {
        if self.cleanup_fails == stage {
            return Err(HostError {
                message: "cleanup rejected".to_string(),
            });
        }
        Ok(())
    }
}

impl ManagedChildHandle for RpcFake {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn session_id(&self) -> Option<String> {
        Some("respawned-session".to_string())
    }
    fn pid(&self) -> Option<i64> {
        Some(4321)
    }
    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        lock(&self.follow_ups).push(text.to_string());
        Ok(())
    }
    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| {})
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        RunnerOutcome::completed("")
    }
    fn switch_session(
        &self,
        _session_path: &str,
    ) -> Option<Result<RpcSwitchSessionResult, HostError>> {
        Some(Ok(RpcSwitchSessionResult {
            cancelled: self.switch_cancelled,
        }))
    }
    fn last_assistant_text(&self) -> Option<String> {
        None
    }
    fn has_terminate(&self) -> bool {
        true
    }
    fn terminate(&self) -> Result<(), HostError> {
        self.rejects(CleanupStage::Terminate)
    }
    fn dispose(&self) -> Result<(), HostError> {
        *lock(&self.dispose_calls) += 1;
        self.rejects(CleanupStage::Dispose)
    }
}

/// `rpcRespawnRunner`: returns the fake and captures the launched spec.
struct CapturingRpcRunner {
    handle: Arc<RpcFake>,
    spec: Mutex<Option<RpcRunnerSpec>>,
}

impl CapturingRpcRunner {
    fn new(handle: Arc<RpcFake>) -> Arc<Self> {
        Arc::new(Self {
            handle,
            spec: Mutex::default(),
        })
    }

    fn spec(&self) -> RpcRunnerSpec {
        lock(&self.spec).clone().expect("rpc runner started")
    }
}

impl RpcRespawnRunner for CapturingRpcRunner {
    fn start(&self, spec: &RpcRunnerSpec) -> ManagedRunnerResult {
        *lock(&self.spec) = Some(spec.clone());
        Ok(Arc::clone(&self.handle) as Arc<dyn ManagedChildHandle>)
    }
}

/// An in-process runner whose `resume` captures the spec and session path.
#[derive(Default)]
struct ResumingRunner {
    resumed: Mutex<Option<(ManagedStartSpec, String)>>,
    resume_calls: Mutex<usize>,
}

impl ManagedRunner for ResumingRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        Ok(FakeHandle::new(&spec.task_id, None) as Arc<dyn ManagedChildHandle>)
    }

    fn resume(&self, spec: &ManagedStartSpec, session_path: &str) -> Option<ManagedRunnerResult> {
        *lock(&self.resume_calls) += 1;
        *lock(&self.resumed) = Some((spec.clone(), session_path.to_string()));
        Some(Ok(
            FakeHandle::new(&spec.task_id, None) as Arc<dyn ManagedChildHandle>
        ))
    }
}

/// The raw in-process runner under a managed adapter; any call is a test failure.
#[derive(Clone, Default)]
struct GuardedChildRunner {
    resume_calls: Arc<Mutex<usize>>,
}

impl InProcessRunnerLike for GuardedChildRunner {
    fn start(&self, _spec: &ChildSpec) -> InProcessHandleResult {
        panic!("start must not be called during respawn");
    }

    fn resume(&self, _spec: &ChildSpec, _session_path: &str) -> Option<InProcessHandleResult> {
        *lock(&self.resume_calls) += 1;
        panic!("resume must be guarded before child spawn");
    }
}

struct Setup {
    project: Project,
    in_process: Option<Arc<dyn ManagedRunner>>,
    rpc: Option<Arc<dyn RpcRespawnRunner>>,
    trusted: Option<TrustedRespawnLaunch>,
    host_pid: Option<i64>,
}

impl Setup {
    fn new() -> Self {
        Self {
            project: Project::new(),
            in_process: None,
            rpc: None,
            trusted: None,
            host_pid: None,
        }
    }

    fn build(self) -> (TaskManager, Project) {
        let fake: Arc<dyn ManagedRunner> = FakeRunner::new();
        let mut options = TaskManagerOptions::new(
            self.project.store(),
            ManagedRunners {
                in_process: self.in_process.unwrap_or_else(|| Arc::clone(&fake)),
                process: fake,
            },
            category_planner(&[]),
            self.project.cwd(),
        );
        options.config = config(5, 1);
        options.rpc_respawn_runner = self.rpc;
        options.host_pid = self.host_pid;
        if let Some(trusted) = self.trusted {
            options.trusted_respawn_launch = Some(Arc::new(move |_record: &TaskRecord| {
                Ok(Some(trusted.clone()))
            }));
        }
        (create_task_manager(options), self.project)
    }
}

fn expect_ok(result: RespawnResult) -> Arc<dyn ManagedChildHandle> {
    match result {
        RespawnResult::Ok(handle) => handle,
        RespawnResult::Failed { code, reason, .. } => {
            panic!("expected ok respawn, got {code:?}: {reason}")
        }
    }
}

fn expect_failed(result: RespawnResult) -> (RespawnDisposition, RespawnFailureCode, String) {
    match result {
        RespawnResult::Ok(_) => panic!("expected failed respawn"),
        RespawnResult::Failed {
            disposition,
            code,
            reason,
        } => (disposition, code, reason),
    }
}

fn session_path() -> &'static Path {
    Path::new("/tmp/session.jsonl")
}

// describe.each(["terminate", "dispose"]) respawn cleanup

#[test]
fn given_cancelled_respawn_cleanup_rejects_when_respawn_returns_then_teardown_failure_surfaced() {
    for stage in [CleanupStage::Terminate, CleanupStage::Dispose] {
        let handle = RpcFake::new(stage, true);
        let mut setup = Setup::new();
        setup.rpc = Some(CapturingRpcRunner::new(Arc::clone(&handle)));
        let (manager, _project) = setup.build();

        let failed = expect_failed(manager.respawn(&respawn_record(), Some(session_path())));

        assert_eq!(
            failed,
            (
                RespawnDisposition::Retryable,
                RespawnFailureCode::RespawnFailed,
                "rpc respawn cleanup failed".to_string()
            )
        );
        assert_eq!(*lock(&handle.dispose_calls), 1);
    }
}

#[test]
fn given_persisted_extension_and_member_env_when_respawned_then_neither_reaches_runner() {
    let record = TaskRecord {
        spawn_spec: Some(legacy_spawn(
            "/tmp/project",
            Some(vec!["/tmp/malicious-extension.ts".to_string()]),
            Some(vec![(
                "MALICIOUS_MEMBER_ENV".to_string(),
                "execute-me".to_string(),
            )]),
        )),
        ..respawn_record()
    };
    let runner = CapturingRpcRunner::new(RpcFake::new(CleanupStage::None, false));
    let mut setup = Setup::new();
    setup.rpc = Some(Arc::clone(&runner) as Arc<dyn RpcRespawnRunner>);
    let (manager, _project) = setup.build();

    expect_ok(manager.respawn(&record, Some(session_path())));

    let spec = runner.spec();
    assert_eq!(spec.extensions, None);
    assert_eq!(spec.member_env, None);
}

#[test]
fn given_team_member_record_when_respawned_then_trusted_launch_replaces_persisted_inputs() {
    let record = TaskRecord {
        name: Some("team:11111111-1111-4111-8111-111111111111:alpha".to_string()),
        spawn_spec: Some(legacy_spawn(
            "/tmp/project",
            Some(vec!["/tmp/malicious-extension.ts".to_string()]),
            Some(vec![(
                "SENPI_TASK_MEMBER".to_string(),
                "untrusted::member".to_string(),
            )]),
        )),
        ..respawn_record()
    };
    let trusted = TrustedRespawnLaunch {
        extensions: Some(vec![
            "/trusted/member-extension.js".to_string(),
            "/trusted/provider-extension.js".to_string(),
        ]),
        member_env: Some(BTreeMap::from([
            (
                "SENPI_TASK_MEMBER".to_string(),
                "11111111-1111-4111-8111-111111111111::alpha".to_string(),
            ),
            (
                "SENPI_TASK_MEMBER_TASK_ID".to_string(),
                record.task_id.clone(),
            ),
            (
                "SENPI_TASK_TEAM_CONFIG".to_string(),
                r#"{"members":["alpha"]}"#.to_string(),
            ),
        ])),
    };
    let runner = CapturingRpcRunner::new(RpcFake::new(CleanupStage::None, false));
    let mut setup = Setup::new();
    setup.rpc = Some(Arc::clone(&runner) as Arc<dyn RpcRespawnRunner>);
    setup.trusted = Some(trusted.clone());
    let (manager, _project) = setup.build();

    expect_ok(manager.respawn(&record, Some(session_path())));

    let spec = runner.spec();
    assert_eq!(spec.extensions, trusted.extensions);
    assert_eq!(spec.member_env, trusted.member_env);
}

#[test]
fn given_record_with_resolved_variant_when_respawned_then_variant_reaches_rpc_spec() {
    let record = TaskRecord {
        resolved_model: Some(ResolvedModelRecord {
            variant: Some("xhigh".to_string()),
            ..agent_model("openai", "gpt-5.6-sol", "openai/gpt-5.6-sol")
        }),
        ..respawn_record()
    };
    let runner = CapturingRpcRunner::new(RpcFake::new(CleanupStage::None, false));
    let mut setup = Setup::new();
    setup.rpc = Some(Arc::clone(&runner) as Arc<dyn RpcRespawnRunner>);
    let (manager, _project) = setup.build();

    expect_ok(manager.respawn(&record, Some(session_path())));

    assert_eq!(runner.spec().variant.as_deref(), Some("xhigh"));
}

// in-process respawn

#[test]
fn given_claimed_in_process_record_when_respawned_then_resume_receives_rebuilt_spec() {
    let setup_project = Project::new();
    let store = setup_project.store();
    store.list().expect("warm list");
    let record = TaskRecord {
        execution_mode: "in-process".to_string(),
        host_pid: Some(HOST_PID),
        resolved_model: Some(agent_model("anthropic", "claude", "Claude")),
        tool_allow: Some(vec!["read".to_string()]),
        tool_deny: Some(vec!["write".to_string()]),
        spawn_spec: Some(TaskSpawnSpec::V1(SpawnSpecV1 {
            cwd: setup_project.cwd(),
            prompt: "persisted effective prompt".to_string(),
            instructions: Some("persisted instructions".to_string()),
            member_scoped_tool_names: Some(vec!["team_ping".to_string()]),
        })),
        ..respawn_record()
    };
    store.replace(&record).expect("seed record");
    let runner = Arc::new(ResumingRunner::default());
    let mut setup = Setup::new();
    setup.project = setup_project;
    setup.in_process = Some(Arc::clone(&runner) as Arc<dyn ManagedRunner>);
    setup.host_pid = Some(HOST_PID);
    let (manager, _project) = setup.build();

    expect_ok(manager.respawn(&record, Some(Path::new("/tmp/in-process-session.jsonl"))));

    let (spec, path) = lock(&runner.resumed).clone().expect("resume called");
    assert_eq!(path, "/tmp/in-process-session.jsonl");
    assert_eq!(spec.task_id, record.task_id);
    assert_eq!(spec.prompt, "persisted effective prompt");
    assert_eq!(spec.instructions.as_deref(), Some("persisted instructions"));
    assert_eq!(spec.resolved_model, record.resolved_model);
    assert_eq!(spec.tool_allowlist, Some(vec!["read".to_string()]));
    assert_eq!(spec.tool_denylist, Some(vec!["write".to_string()]));
    assert_eq!(
        spec.member_scoped_tool_names,
        Some(vec!["team_ping".to_string()])
    );
}

#[test]
fn given_resume_context_cannot_find_model_when_respawned_then_retryable_and_no_child_spawned() {
    let setup = Setup::new();
    let record = TaskRecord {
        execution_mode: "in-process".to_string(),
        resolved_model: Some(agent_model(
            "removed-provider",
            "removed-model",
            "Removed Model",
        )),
        spawn_spec: Some(TaskSpawnSpec::V1(SpawnSpecV1 {
            cwd: setup.project.cwd(),
            prompt: "continue safely".to_string(),
            instructions: None,
            member_scoped_tool_names: None,
        })),
        ..respawn_record()
    };
    let child = GuardedChildRunner::default();
    let managed = create_in_process_managed_runner(
        child.clone(),
        create_parent_registry_session_context(Arc::new(|| None)),
    );
    let mut setup = setup;
    setup.in_process = Some(managed);
    let (manager, _project) = setup.build();

    let failed = expect_failed(manager.respawn(&record, Some(session_path())));

    assert_eq!(
        failed,
        (
            RespawnDisposition::Retryable,
            RespawnFailureCode::ModelUnavailable,
            "no live parent model registry available".to_string()
        )
    );
    assert_eq!(*lock(&child.resume_calls), 0);
}

#[test]
fn given_in_process_record_without_v1_facts_when_respawned_then_unrecoverable_without_spawning() {
    let record = TaskRecord {
        execution_mode: "in-process".to_string(),
        ..respawn_record()
    };
    let runner = Arc::new(ResumingRunner::default());
    let mut setup = Setup::new();
    setup.in_process = Some(Arc::clone(&runner) as Arc<dyn ManagedRunner>);
    let (manager, _project) = setup.build();

    let failed = expect_failed(manager.respawn(&record, Some(session_path())));

    assert_eq!(
        failed,
        (
            RespawnDisposition::Unrecoverable,
            RespawnFailureCode::SpawnSpecUnavailable,
            "record has no persisted v1 spawn_spec to rebuild from".to_string()
        )
    );
    assert_eq!(*lock(&runner.resume_calls), 0);
}

// guarded reattach

fn reattach_setup(record: &TaskRecord) -> (TaskManager, Project) {
    let setup = Setup {
        host_pid: Some(HOST_PID),
        ..Setup::new()
    };
    let store = setup.project.store();
    store.list().expect("warm list");
    store.replace(record).expect("seed record");
    setup.build()
}

#[test]
fn given_no_prior_host_ownership_claim_when_reattached_then_handle_rejected() {
    let record = TaskRecord {
        host_pid: None,
        ..respawn_record()
    };
    let (manager, _project) = reattach_setup(&record);

    let result = manager.reattach(&record, FakeHandle::new(&record.task_id, None));

    assert_eq!(
        result,
        ReattachResult::Failed {
            kind: ReattachFailureKind::Failed,
            reason: "task ownership claim is not held by this host".to_string(),
        }
    );
}

#[test]
fn given_claimed_non_terminal_record_with_stale_output_when_reattached_then_output_clears_and_epoch_bumps()
 {
    let record = TaskRecord {
        status: TaskStatus::Running,
        error_message: Some("stale error".to_string()),
        final_response: Some("stale response".to_string()),
        host_pid: Some(HOST_PID),
        notification: TaskNotification {
            run_epoch: 4,
            notified_epoch: 3,
            ..TaskNotification::default()
        },
        ..respawn_record()
    };
    let (manager, project) = reattach_setup(&record);

    let result = manager.reattach(&record, FakeHandle::new(&record.task_id, None));

    assert_eq!(result, ReattachResult::Ok);
    let stored = project
        .store()
        .load(&record.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(stored.status, TaskStatus::Running);
    assert_eq!(stored.notification.run_epoch, 5);
    assert_eq!(stored.notification.notified_epoch, 3);
    assert_eq!(stored.error_message, None);
    assert_eq!(stored.final_response, None);
}

#[test]
fn given_claimed_terminal_record_when_reattached_then_run_epoch_unchanged() {
    let record = TaskRecord {
        status: TaskStatus::Completed,
        final_response: Some("done".to_string()),
        host_pid: Some(HOST_PID),
        notification: TaskNotification {
            run_epoch: 4,
            notified_epoch: 3,
            ..TaskNotification::default()
        },
        ..respawn_record()
    };
    let (manager, project) = reattach_setup(&record);

    let result = manager.reattach(&record, FakeHandle::new(&record.task_id, None));

    assert_eq!(result, ReattachResult::Ok);
    let stored = project
        .store()
        .load(&record.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(stored.notification.run_epoch, 4);
}

// respawn continuation

struct Continuation {
    manager: TaskManager,
    handle: Arc<RpcFake>,
    project: Project,
}

fn continuation_harness() -> Continuation {
    let handle = RpcFake::new(CleanupStage::None, false);
    let mut setup = Setup::new();
    setup.rpc = Some(CapturingRpcRunner::new(Arc::clone(&handle)));
    let (manager, project) = setup.build();
    Continuation {
        manager,
        handle,
        project,
    }
}

fn write_session(project: &Project, lines: &[String]) -> std::path::PathBuf {
    let path = project.dir.path().join("session.jsonl");
    std::fs::write(&path, lines.join("\n")).expect("write session");
    path
}

fn follow_ups_after(record: &TaskRecord, lines: &[&str]) -> usize {
    let harness = continuation_harness();
    let lines: Vec<String> = lines.iter().map(|line| (*line).to_string()).collect();
    let path = write_session(&harness.project, &lines);
    expect_ok(harness.manager.respawn(record, Some(&path)));
    lock(&harness.handle.follow_ups).len()
}

#[test]
fn given_tool_result_tail_when_respawned_then_revived_child_gets_continuation() {
    let count = follow_ups_after(
        &respawn_record(),
        &[
            r#"{"type":"session","version":3,"id":"s"}"#,
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"half done"}]}}"#,
            r#"{"type":"message","message":{"role":"toolResult","content":[{"type":"text","text":"tool output"}]}}"#,
        ],
    );

    assert_eq!(count, 1);
}

#[test]
fn given_assistant_tool_call_tail_when_respawned_then_revived_child_gets_continuation() {
    let count = follow_ups_after(
        &respawn_record(),
        &[
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash","arguments":{}}]}}"#,
        ],
    );

    assert_eq!(count, 1);
}

#[test]
fn given_trailing_user_message_when_respawned_then_exactly_one_continuation() {
    let count = follow_ups_after(
        &respawn_record(),
        &[
            r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"continue"}]}}"#,
        ],
    );

    assert_eq!(count, 1);
}

#[test]
fn given_aborted_assistant_tail_when_respawned_then_exactly_one_continuation() {
    let count = follow_ups_after(
        &respawn_record(),
        &[
            r#"{"type":"message","message":{"role":"assistant","stopReason":"aborted","content":[{"type":"text","text":"partial"}]}}"#,
        ],
    );

    assert_eq!(count, 1);
}

#[test]
fn given_terminal_record_with_user_tail_when_respawned_then_no_continuation() {
    let record = TaskRecord {
        status: TaskStatus::Completed,
        final_response: Some("done".to_string()),
        ..respawn_record()
    };

    let count = follow_ups_after(
        &record,
        &[
            r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"stale"}]}}"#,
        ],
    );

    assert_eq!(count, 0);
}

#[test]
fn given_completed_assistant_line_over_64_kib_when_respawned_then_no_false_positive_continuation() {
    let large = serde_json::json!({
        "type": "message",
        "message": { "role": "assistant", "content": [{ "type": "text", "text": "x".repeat(70 * 1024) }] },
    })
    .to_string();

    let count = follow_ups_after(
        &respawn_record(),
        &[
            r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"work"}]}}"#,
            &large,
        ],
    );

    assert_eq!(count, 0);
}

#[test]
fn given_assistant_text_only_tail_when_respawned_then_no_continuation() {
    let count = follow_ups_after(
        &respawn_record(),
        &[
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"finished"},{"type":"thinking","thinking":"done"}]}}"#,
        ],
    );

    assert_eq!(count, 0);
}

#[test]
fn given_unreadable_session_path_when_respawned_then_no_continuation_and_respawn_succeeds() {
    let harness = continuation_harness();

    expect_ok(harness.manager.respawn(
        &respawn_record(),
        Some(Path::new("/tmp/definitely-missing-session.jsonl")),
    ));

    assert!(lock(&harness.handle.follow_ups).is_empty());
}
