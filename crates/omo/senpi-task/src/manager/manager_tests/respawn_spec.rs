//! `manager/manager-respawn-spec.test.ts`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use super::fakes::{
    FakeHandle, FakeRunner, Harness, HarnessOptions, base_spec, lock, make_manager, started,
    wait_until,
};
use crate::host::HostError;
use crate::manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::helpers::{
    SPAWN_SPEC_UNAVAILABLE_REASON, build_record_input, build_respawn_managed_spec, child_state_dir,
};
use crate::manager::types::{ChildPlanner, ManagedStartSpec, ManagerStartSpec, ResolvedChildPlan};
use crate::runners::RunnerOutcome;
use crate::runners::in_process::shared_tool_filter::{ChildTool, ChildToolRef};
use crate::runners::types::RpcSpawnSpec;
use crate::state::{
    ResolvedModelRecord, ResolvedModelSource, SpawnSpecV1, TaskRecord, TaskRecordInput,
    TaskSpawnSpec, create_task_record,
};

struct NamedTool(String);

impl ChildTool for NamedTool {
    fn name(&self) -> &str {
        &self.0
    }

    fn description(&self) -> &str {
        "test tool"
    }

    fn execute(&self, _tool_call_id: &str, _input: &Value) -> Result<Value, HostError> {
        Ok(serde_json::json!({ "content": [{ "type": "text", "text": "ok" }] }))
    }
}

fn tool(name: &str) -> ChildToolRef {
    Arc::new(NamedTool(name.to_string()))
}

fn resolved() -> ResolvedModelRecord {
    ResolvedModelRecord {
        display: "Claude Opus 4".to_string(),
        variant: Some("high".to_string()),
        ..ResolvedModelRecord::new(ResolvedModelSource::Category, "anthropic", "claude-opus-4")
    }
}

fn plan() -> ResolvedChildPlan {
    ResolvedChildPlan {
        model: "anthropic/claude-opus-4".to_string(),
        resolved_model: Some(resolved()),
        variant: Some("high".to_string()),
        prompt_append: Some("PLANNER APPEND".to_string()),
        instructions: Some("planner instructions".to_string()),
        tool_allowlist: Some(vec!["read".to_string(), "bash".to_string()]),
        tool_denylist: Some(vec!["write".to_string(), "edit".to_string()]),
        ..ResolvedChildPlan::default()
    }
}

fn full_planner() -> ChildPlanner {
    let plan = plan();
    Arc::new(move |_spec: &ManagerStartSpec| Ok(plan.clone()))
}

fn harness(in_process: Option<Arc<FakeRunner>>, process: Option<Arc<FakeRunner>>) -> Harness {
    make_manager(HarnessOptions {
        planner: Some(full_planner()),
        in_process,
        process,
        ..HarnessOptions::default()
    })
}

fn persisted_v1(harness: &Harness, task_id: &str) -> SpawnSpecV1 {
    let record = harness.store.load(task_id).expect("load").expect("record");
    match record.spawn_spec {
        Some(TaskSpawnSpec::V1(spec)) => spec,
        other => panic!("expected a v1 spawn spec, got {other:?}"),
    }
}

fn plain_record(execution_mode: &str) -> TaskRecord {
    create_task_record(
        TaskRecordInput {
            parent_session_id: "parent-1".into(),
            root_session_id: "parent-1".into(),
            depth: 1,
            execution_mode: execution_mode.into(),
            model: "anthropic/claude-opus-4".into(),
            ..TaskRecordInput::default()
        },
        None,
    )
    .expect("record")
}

/// `EchoRunner`: a handle that echoes launch facts (extensions, member env) as its spawn spec.
struct EchoHandle {
    inner: Arc<FakeHandle>,
    cwd: String,
}

impl ManagedChildHandle for EchoHandle {
    fn task_id(&self) -> &str {
        self.inner.task_id()
    }
    fn session_id(&self) -> Option<String> {
        self.inner.session_id()
    }
    fn pid(&self) -> Option<i64> {
        self.inner.pid()
    }
    fn spawn_spec(&self) -> Option<RpcSpawnSpec> {
        Some(RpcSpawnSpec {
            cwd: self.cwd.clone(),
            extensions: Some(vec!["/tmp/echo-ext.ts".to_string()]),
            member_env: Some(BTreeMap::from([("ECHO".to_string(), "1".to_string())])),
        })
    }
    fn steer(&self, text: &str) -> Result<(), HostError> {
        self.inner.steer(text)
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        self.inner.follow_up(text)
    }
    fn abort(&self) -> Result<(), HostError> {
        self.inner.abort()
    }
    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        self.inner.subscribe(listener)
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        self.inner.wait_for_outcome()
    }
    fn last_assistant_text(&self) -> Option<String> {
        self.inner.last_assistant_text()
    }
    fn dispose(&self) -> Result<(), HostError> {
        self.inner.dispose()
    }
}

#[test]
fn given_in_process_spawn_with_append_and_member_tools_when_started_then_v1_has_effective_prompt() {
    let harness = harness(None, None);

    let task = started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(ExecutionMode::InProcess),
        prompt: "RAW PROMPT".to_string(),
        instructions: Some("be careful".to_string()),
        member_scoped_tools: Some(vec![tool("alpha_read"), tool("alpha_write")]),
        ..base_spec()
    }));

    let persisted = persisted_v1(&harness, &task.task_id);
    assert_eq!(persisted.prompt, "RAW PROMPT\n\nPLANNER APPEND");
    assert_eq!(persisted.cwd, harness.project.cwd());
    assert_eq!(persisted.instructions.as_deref(), Some("be careful"));
    assert_eq!(
        persisted.member_scoped_tool_names,
        Some(vec!["alpha_read".to_string(), "alpha_write".to_string()])
    );
}

#[test]
fn given_process_spawn_with_extensions_and_member_env_when_started_then_v1_carries_neither() {
    let harness = harness(None, None);

    let task = started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(ExecutionMode::Process),
        extensions: Some(vec!["/tmp/member-extension.ts".to_string()]),
        member_env: Some(BTreeMap::from([(
            "SENPI_TASK_MEMBER".to_string(),
            "run-1::alpha".to_string(),
        )])),
        ..base_spec()
    }));

    let persisted = persisted_v1(&harness, &task.task_id);
    assert_eq!(persisted.prompt, "do the thing\n\nPLANNER APPEND");
    let json = serde_json::to_value(TaskSpawnSpec::V1(persisted)).expect("json");
    assert!(json.get("extensions").is_none(), "{json}");
    assert!(json.get("member_env").is_none(), "{json}");
}

#[test]
fn given_rpc_child_echoing_launch_spec_when_spawn_facts_recorded_then_v1_not_clobbered() {
    let runner = FakeRunner::new();
    let inner = Arc::new(std::sync::Mutex::new(None::<Arc<FakeHandle>>));
    let slot = Arc::clone(&inner);
    *lock(&runner.hook) = Some(Arc::new(move |spec: &ManagedStartSpec, _call| {
        let fake = FakeHandle::new(&spec.task_id, None);
        *lock(&slot) = Some(Arc::clone(&fake));
        let handle: Arc<dyn ManagedChildHandle> = Arc::new(EchoHandle {
            inner: fake,
            cwd: spec.cwd.clone(),
        });
        Some(Ok(handle))
    }));
    let harness = harness(None, Some(Arc::clone(&runner)));

    let task = started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(ExecutionMode::Process),
        prompt: "RAW PROMPT".to_string(),
        ..base_spec()
    }));
    // Spawn facts are written before the manager subscribes to the handle.
    wait_until("spawn facts recorded", || {
        lock(&inner)
            .as_ref()
            .is_some_and(|fake| fake.subscribe_count() > 0)
    });

    let persisted = persisted_v1(&harness, &task.task_id);
    assert_eq!(persisted.prompt, "RAW PROMPT\n\nPLANNER APPEND");
}

#[test]
fn given_spawned_in_process_record_when_respawn_spec_built_then_matches_original_minus_runtime_objects()
 {
    let runner = FakeRunner::new();
    let harness = harness(Some(Arc::clone(&runner)), None);
    let task = started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(ExecutionMode::InProcess),
        prompt: "RAW PROMPT".to_string(),
        subagent_type: Some("explore".to_string()),
        member_scoped_tools: Some(vec![tool("alpha_read")]),
        ..base_spec()
    }));
    let original = runner.specs().into_iter().next().expect("start spec");
    let record = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");

    let rebuilt =
        build_respawn_managed_spec(&record, harness.store.state_dir()).expect("rebuilt spec");

    let expected = ManagedStartSpec {
        task_id: original.task_id.clone(),
        cwd: original.cwd.clone(),
        state_dir: child_state_dir(harness.store.state_dir(), &task.task_id),
        prompt: "RAW PROMPT\n\nPLANNER APPEND".to_string(),
        depth: original.depth,
        parent_session_id: original.parent_session_id.clone(),
        root_session_id: original.root_session_id.clone(),
        model: original.model.clone(),
        resolved_model: Some(resolved()),
        variant: Some("high".to_string()),
        agent_type: Some("explore".to_string()),
        instructions: Some("planner instructions".to_string()),
        tool_allowlist: Some(vec!["read".to_string(), "bash".to_string()]),
        tool_denylist: Some(vec!["write".to_string(), "edit".to_string()]),
        member_scoped_tool_names: Some(vec!["alpha_read".to_string()]),
        ..ManagedStartSpec::default()
    };
    assert_eq!(rebuilt.state_dir, expected.state_dir);
    assert!(rebuilt.member_scoped_tools.is_none());
    assert!(rebuilt.extensions.is_none());
    assert!(rebuilt.member_env.is_none());
    assert_eq!(rebuilt.task_id, expected.task_id);
    assert_eq!(rebuilt.cwd, expected.cwd);
    assert_eq!(rebuilt.prompt, expected.prompt);
    assert_eq!(rebuilt.depth, expected.depth);
    assert_eq!(rebuilt.parent_session_id, expected.parent_session_id);
    assert_eq!(rebuilt.root_session_id, expected.root_session_id);
    assert_eq!(rebuilt.model, expected.model);
    assert_eq!(rebuilt.requested_model, expected.requested_model);
    assert_eq!(rebuilt.fallback_models, expected.fallback_models);
    assert_eq!(rebuilt.resolved_model, expected.resolved_model);
    assert_eq!(rebuilt.variant, expected.variant);
    assert_eq!(rebuilt.agent_type, expected.agent_type);
    assert_eq!(rebuilt.instructions, expected.instructions);
    assert_eq!(rebuilt.tool_allowlist, expected.tool_allowlist);
    assert_eq!(rebuilt.tool_denylist, expected.tool_denylist);
    assert_eq!(
        rebuilt.member_scoped_tool_names,
        expected.member_scoped_tool_names
    );
}

#[test]
fn given_record_without_spawn_spec_when_respawn_spec_built_then_spawn_spec_unavailable() {
    let rebuilt = build_respawn_managed_spec(&plain_record("in-process"), Path::new("/tmp/state"));

    assert_eq!(rebuilt.err(), Some(SPAWN_SPEC_UNAVAILABLE_REASON));
}

#[test]
fn given_legacy_cwd_spawn_spec_when_respawn_spec_built_then_spawn_spec_unavailable() {
    let record = TaskRecord {
        spawn_spec: Some(TaskSpawnSpec::LegacyProcess {
            cwd: "/tmp/project".to_string(),
            extensions: None,
            member_env: None,
        }),
        ..plain_record("process")
    };

    let rebuilt = build_respawn_managed_spec(&record, Path::new("/tmp/state"));

    assert_eq!(rebuilt.err(), Some(SPAWN_SPEC_UNAVAILABLE_REASON));
}

#[test]
fn given_plan_with_tool_denylist_when_record_input_built_then_tool_deny_rides_record() {
    let input = build_record_input(
        &base_spec(),
        &ResolvedChildPlan {
            model: "anthropic/claude-opus-4".to_string(),
            tool_denylist: Some(vec!["write".to_string(), "edit".to_string()]),
            ..ResolvedChildPlan::default()
        },
        "n",
        ExecutionMode::InProcess,
    );

    assert_eq!(
        input.tool_deny,
        Some(vec!["write".to_string(), "edit".to_string()])
    );
}

#[test]
fn given_plan_without_denylist_when_record_input_built_then_tool_deny_absent() {
    let input = build_record_input(
        &base_spec(),
        &ResolvedChildPlan {
            model: "anthropic/claude-opus-4".to_string(),
            ..ResolvedChildPlan::default()
        },
        "n",
        ExecutionMode::InProcess,
    );

    assert_eq!(input.tool_deny, None);
}

#[test]
fn given_run_in_background_when_record_input_built_then_notify_on_terminal_durable() {
    let plan = ResolvedChildPlan {
        model: "anthropic/claude-opus-4".to_string(),
        ..ResolvedChildPlan::default()
    };
    let background = build_record_input(
        &ManagerStartSpec {
            run_in_background: true,
            ..base_spec()
        },
        &plan,
        "n",
        ExecutionMode::InProcess,
    );
    let foreground = build_record_input(&base_spec(), &plan, "n", ExecutionMode::InProcess);

    assert!(background.notify_on_terminal);
    assert!(!foreground.notify_on_terminal);
}
