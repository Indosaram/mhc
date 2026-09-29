//! `manager/runner.test.ts`: the in-process and RPC managed-runner adapters.

use std::sync::{Arc, Mutex};

use super::fakes::FakeHandle;
use crate::manager::child_handle::ManagedChildHandle;
use crate::manager::runner::{
    InProcessHandleResult, InProcessRunnerLike, InProcessSessionContext, RpcRunnerLike,
    create_in_process_managed_runner, create_rpc_managed_runner,
};
use crate::manager::types::ManagedStartSpec;
use crate::runners::in_process::ChildSpec;
use crate::runners::in_process::child_options::HostHandle;
use crate::runners::rpc::spawn::resolve_child_session_dir;
use crate::runners::types::RpcRunnerSpec;
use crate::runners::{RunnerFailure, RunnerOutcome};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

const STATE_DIR: &str = "/tmp/project/.omo/senpi-task/children/st_00000001";

fn managed_spec() -> ManagedStartSpec {
    ManagedStartSpec {
        task_id: "st_00000001".to_string(),
        cwd: "/tmp/project".to_string(),
        state_dir: STATE_DIR.to_string(),
        prompt: "do it".to_string(),
        depth: 1,
        parent_session_id: "parent-1".to_string(),
        root_session_id: "parent-1".to_string(),
        model: Some("anthropic/claude".to_string()),
        ..ManagedStartSpec::default()
    }
}

/// The captured child spec plus the resume path, when the call was a resume.
type Captured = Arc<Mutex<Option<(ChildSpec, Option<String>)>>>;

/// Records the child spec and resume path, answering with a completed fake handle.
#[derive(Default)]
struct CapturingInProcess {
    captured: Captured,
    can_resume: bool,
}

impl CapturingInProcess {
    fn handle(&self, spec: &ChildSpec, path: Option<&str>) -> InProcessHandleResult {
        *self.captured.lock().expect("captured") = Some((spec.clone(), path.map(str::to_string)));
        let handle = FakeHandle::new(&spec.task_id, None);
        handle.complete("ok");
        Ok(handle as Arc<dyn ManagedChildHandle>)
    }
}

impl InProcessRunnerLike for CapturingInProcess {
    fn start(&self, spec: &ChildSpec) -> InProcessHandleResult {
        self.handle(spec, None)
    }

    fn resume(&self, spec: &ChildSpec, session_path: &str) -> Option<InProcessHandleResult> {
        self.can_resume
            .then(|| self.handle(spec, Some(session_path)))
    }
}

#[derive(Default)]
struct CapturingRpc {
    captured: Arc<Mutex<Option<RpcRunnerSpec>>>,
}

impl RpcRunnerLike for CapturingRpc {
    fn start(&self, spec: &RpcRunnerSpec) -> Result<Arc<dyn ManagedChildHandle>, RunnerFailure> {
        *self.captured.lock().expect("captured") = Some(spec.clone());
        Ok(FakeHandle::new(&spec.task_id, Some(99)))
    }
}

fn captured_child(
    captured: &Mutex<Option<(ChildSpec, Option<String>)>>,
) -> (ChildSpec, Option<String>) {
    captured
        .lock()
        .expect("captured")
        .clone()
        .expect("runner called")
}

fn rpc_start(spec: &ManagedStartSpec) -> (RpcRunnerSpec, Arc<dyn ManagedChildHandle>) {
    let runner = CapturingRpc::default();
    let captured = Arc::clone(&runner.captured);
    let handle = create_rpc_managed_runner(runner)
        .start(spec)
        .expect("started");
    let spec = captured
        .lock()
        .expect("captured")
        .clone()
        .expect("runner called");
    (spec, handle)
}

#[test]
fn given_managed_spec_when_in_process_started_then_maps_to_child_spec_and_injects_context() {
    let runner = CapturingInProcess::default();
    let captured = Arc::clone(&runner.captured);
    let model_runtime: HostHandle = Arc::new("native-provider-runtime");
    let context_runtime = Arc::clone(&model_runtime);
    let managed = create_in_process_managed_runner(runner, move |_: &ManagedStartSpec| {
        InProcessSessionContext {
            agent_dir: Some("/home/user/.senpi/agent".to_string()),
            model_runtime: Some(Arc::clone(&context_runtime)),
            ..InProcessSessionContext::default()
        }
    });

    let handle = managed.start(&managed_spec()).expect("started");
    let outcome = handle.wait_for_outcome();

    let (child, _) = captured_child(&captured);
    assert_eq!(child.task_id, "st_00000001");
    assert_eq!(child.agent_dir.as_deref(), Some("/home/user/.senpi/agent"));
    let runtime = child.model_runtime.expect("model runtime");
    assert!(Arc::ptr_eq(&runtime, &model_runtime));
    assert_eq!(child.parent_session_id, "parent-1");
    assert_eq!(
        child.session_dir,
        Some(resolve_child_session_dir(STATE_DIR, "st_00000001"))
    );
    assert_eq!(outcome, RunnerOutcome::completed("ok"));
}

#[test]
fn given_managed_spec_with_runtime_fallback_chain_when_started_then_ordered_chain_reaches_child() {
    let runner = CapturingInProcess::default();
    let captured = Arc::clone(&runner.captured);
    let requested = ResolvedModelRecord {
        reasoning_effort: Some("minimal".to_string()),
        ..ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            "kimi-coding",
            "kimi-for-coding-highspeed-unlocked",
        )
    };
    let fallbacks = vec![ResolvedModelRecord {
        reasoning_effort: Some("minimal".to_string()),
        ..ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            "quotio-openai",
            "gpt-5.6-luna-fast",
        )
    }];
    let managed = create_in_process_managed_runner(runner, |_: &ManagedStartSpec| {
        InProcessSessionContext::default()
    });

    managed
        .start(&ManagedStartSpec {
            requested_model: Some(requested.clone()),
            fallback_models: Some(fallbacks.clone()),
            ..managed_spec()
        })
        .expect("started");

    let (child, _) = captured_child(&captured);
    assert_eq!(child.requested_model, Some(requested));
    assert_eq!(child.fallback_models, Some(fallbacks));
}

#[test]
fn given_managed_resume_spec_when_resumed_then_persisted_tool_and_model_facts_reach_child_spec() {
    let runner = CapturingInProcess {
        can_resume: true,
        ..CapturingInProcess::default()
    };
    let captured = Arc::clone(&runner.captured);
    let managed = create_in_process_managed_runner(runner, |_: &ManagedStartSpec| {
        InProcessSessionContext::default()
    });
    let resolved = ResolvedModelRecord {
        display: "Claude".to_string(),
        ..ResolvedModelRecord::new(ResolvedModelSource::Agent, "anthropic", "claude")
    };

    managed
        .resume(
            &ManagedStartSpec {
                resolved_model: Some(resolved.clone()),
                tool_denylist: Some(vec!["write".to_string()]),
                member_scoped_tool_names: Some(vec!["team_ping".to_string()]),
                ..managed_spec()
            },
            "/tmp/session.jsonl",
        )
        .expect("resume supported")
        .expect("resumed");

    let (child, path) = captured_child(&captured);
    assert_eq!(path.as_deref(), Some("/tmp/session.jsonl"));
    assert_eq!(child.resolved_model, Some(resolved));
    assert_eq!(child.tool_denylist, Some(vec!["write".to_string()]));
    assert_eq!(
        child.member_scoped_tool_names,
        Some(vec!["team_ping".to_string()])
    );
}

#[test]
fn given_managed_spec_when_rpc_started_then_state_dir_mapped_and_handle_adapted() {
    let (spec, handle) = rpc_start(&managed_spec());

    assert_eq!(spec.state_dir, STATE_DIR);
    assert_eq!(spec.prompt, "do it");
    assert_eq!(handle.pid(), Some(99));
}

#[test]
fn given_managed_spec_with_model_when_rpc_started_then_model_threaded_onto_rpc_spec() {
    let (spec, _) = rpc_start(&managed_spec());

    assert_eq!(spec.model.as_deref(), Some("anthropic/claude"));
}

#[test]
fn given_managed_spec_with_variant_when_rpc_started_then_variant_reaches_rpc_spec() {
    let (spec, _) = rpc_start(&ManagedStartSpec {
        variant: Some("max".to_string()),
        ..managed_spec()
    });

    assert_eq!(spec.variant.as_deref(), Some("max"));
}

#[test]
fn given_managed_spec_without_variant_when_rpc_started_then_no_variant_threaded() {
    let (spec, _) = rpc_start(&managed_spec());

    assert_eq!(spec.variant, None);
}

#[test]
fn given_session_context_with_thinking_level_when_in_process_started_then_level_reaches_child_spec()
{
    let runner = CapturingInProcess::default();
    let captured = Arc::clone(&runner.captured);
    let managed =
        create_in_process_managed_runner(runner, |_: &ManagedStartSpec| InProcessSessionContext {
            thinking_level: Some("high".to_string()),
            ..InProcessSessionContext::default()
        });

    managed.start(&managed_spec()).expect("started");

    let (child, _) = captured_child(&captured);
    assert_eq!(child.thinking_level.as_deref(), Some("high"));
}
