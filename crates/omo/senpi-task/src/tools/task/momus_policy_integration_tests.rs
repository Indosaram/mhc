//! `tools/task/momus-policy.integration.test.ts`

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use pretty_assertions::assert_eq;

use crate::agents::{
    PlanArtifactReference, SkillInvocationState, interaction_policy_for_agent,
};
use crate::host::HostError;
use crate::manager::concurrency::TaskConcurrencyConfig;
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{
    ChildPlanner, ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec,
    ManagerConfig, ManagerStartSpec, ResolvedChildPlan, TaskManagerOptions,
};
use crate::manager::{
    ManagedChildHandle, ManagedChildListener, TaskManager, Unsubscribe, create_task_manager,
};
use crate::runners::RunnerOutcome;
use crate::state::DeliverAs;
use crate::steering::{SendInput, SendOutcome};
use crate::store::{StateDirConfig, TaskRecordStore};
use crate::tools::task::execute::build_task_execute;
use crate::tools::task::execute_single::TaskExecuteDeps;
use crate::tools::task::execute_spec::TaskToolDeps;
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::plan_review_contract::resolve_plan_review_contract;
use crate::tools::task::spawn_policy::{PlanReviewContractOutcome, SpawnPolicyDeps};
use crate::tools::task::types::SkillResolution;
use crate::tools::task::validation::SpawnParamsInput;

const PLAN_A: &str = ".omo/plans/alpha-plan.md";
const PLAN_B: &str = ".omo/plans/beta-plan.md";
const WAIT: Duration = Duration::from_secs(5);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn canonical(path: &str) -> String {
    format!("Review the work plan at {path} for contradictions and blocking issues.")
}

// ---------------------------------------------------------------------------
// Minimal manager fakes (port of the parts of manager/__fixtures__/manager-fakes.ts used here)
// ---------------------------------------------------------------------------

struct FakeHandle {
    task_id: String,
    steer_calls: Mutex<Vec<String>>,
    outcomes: Mutex<Receiver<RunnerOutcome>>,
    settle: Mutex<Sender<RunnerOutcome>>,
}

impl FakeHandle {
    fn new(task_id: &str) -> Arc<Self> {
        let (sender, receiver) = channel();
        Arc::new(Self {
            task_id: task_id.to_string(),
            steer_calls: Mutex::new(Vec::new()),
            outcomes: Mutex::new(receiver),
            settle: Mutex::new(sender),
        })
    }

    fn complete(&self, final_response: &str) {
        lock(&self.settle)
            .send(RunnerOutcome::Completed {
                final_response: final_response.to_string(),
            })
            .expect("outcome receiver alive");
    }

    fn steer_calls(&self) -> Vec<String> {
        lock(&self.steer_calls).clone()
    }
}

impl ManagedChildHandle for FakeHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }

    fn session_id(&self) -> Option<String> {
        Some(format!("sess-{}", self.task_id))
    }

    fn pid(&self) -> Option<i64> {
        None
    }

    fn steer(&self, text: &str) -> Result<(), HostError> {
        lock(&self.steer_calls).push(text.to_string());
        Ok(())
    }

    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        lock(&self.steer_calls).push(text.to_string());
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }

    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| {})
    }

    fn wait_for_outcome(&self) -> RunnerOutcome {
        lock(&self.outcomes)
            .recv()
            .unwrap_or(RunnerOutcome::Cancelled)
    }

    fn last_assistant_text(&self) -> Option<String> {
        None
    }

    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeRunner {
    handles: Mutex<HashMap<String, Arc<FakeHandle>>>,
    started_specs: Mutex<Vec<ManagedStartSpec>>,
    changed: Condvar,
}

impl FakeRunner {
    fn handle(&self, task_id: &str) -> Option<Arc<FakeHandle>> {
        lock(&self.handles).get(task_id).cloned()
    }

    fn wait_handle(&self, task_id: &str) -> Arc<FakeHandle> {
        let deadline = Instant::now() + WAIT;
        let mut handles = lock(&self.handles);
        loop {
            if let Some(handle) = handles.get(task_id) {
                return Arc::clone(handle);
            }
            let now = Instant::now();
            assert!(now < deadline, "timed out waiting for handle {task_id}");
            handles = self
                .changed
                .wait_timeout(handles, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn specs(&self) -> Vec<ManagedStartSpec> {
        lock(&self.started_specs).clone()
    }
}

impl ManagedRunner for FakeRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        lock(&self.started_specs).push(spec.clone());
        let fake = FakeHandle::new(&spec.task_id);
        lock(&self.handles).insert(spec.task_id.clone(), Arc::clone(&fake));
        self.changed.notify_all();
        Ok(fake)
    }
}

fn planner() -> ChildPlanner {
    Arc::new(|spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: spec
                .model
                .clone()
                .unwrap_or_else(|| "anthropic/claude".to_string()),
            category: spec.category.clone(),
            agent_type: spec.subagent_type.clone(),
            ..ResolvedChildPlan::default()
        })
    })
}

struct Harness {
    manager: TaskManager,
    store: TaskRecordStore,
    in_process: Arc<FakeRunner>,
    _dir: tempfile::TempDir,
}

fn make_manager() -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: dir.path().to_path_buf(),
        task_state_dir: None,
    });
    let in_process = Arc::new(FakeRunner::default());
    let process = Arc::new(FakeRunner::default());
    let mut options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: Arc::clone(&in_process) as Arc<dyn ManagedRunner>,
            process: process as Arc<dyn ManagedRunner>,
        },
        planner(),
        dir.path().to_string_lossy().into_owned(),
    );
    options.config = ManagerConfig {
        concurrency: TaskConcurrencyConfig {
            default_concurrency: Some(5),
            provider_concurrency: None,
            model_concurrency: None,
        },
        max_depth: 1,
        default_execution_mode: ExecutionMode::InProcess,
    };
    Harness {
        manager: create_task_manager(options),
        store,
        in_process,
        _dir: dir,
    }
}

// ---------------------------------------------------------------------------
// Session policy fakes
// ---------------------------------------------------------------------------

/// A session where the user requested `ulw-plan` and plan A was referenced 3x, plan B 1x.
struct OpenPlanSession;

impl SkillInvocationState for OpenPlanSession {
    fn has_invoked(&self, _skill: &str) -> bool {
        false
    }

    fn has_user_requested(&self, skill: &str) -> bool {
        skill == "ulw-plan"
    }

    fn has_plan_artifact(&self) -> bool {
        true
    }

    fn plan_artifact_references(&self) -> Vec<PlanArtifactReference> {
        vec![
            PlanArtifactReference {
                path: PLAN_A.to_string(),
                count: 3,
                last_touched_at: 2,
            },
            PlanArtifactReference {
                path: PLAN_B.to_string(),
                count: 1,
                last_touched_at: 1,
            },
        ]
    }
}

/// `openSessionResolver()`: every session resolves to the open plan session.
struct OpenSessionPolicy;

impl SpawnPolicyDeps for OpenSessionPolicy {
    fn invocation_gate_denial(&self, _subagent_type: &str, _session_id: &str) -> Option<String> {
        // The open session requested `ulw-plan` and holds a plan artifact, so the gate admits.
        None
    }

    fn plan_review_contract_outcome(
        &self,
        subagent_type: &str,
        caller_prompt: &str,
        _session_id: &str,
    ) -> Option<PlanReviewContractOutcome> {
        resolve_plan_review_contract(subagent_type, caller_prompt, &OpenPlanSession)
    }
}

fn ctx() -> TaskToolContext {
    TaskToolContext {
        cwd: "/work/project".to_string(),
        session_id: "parent-session-1".to_string(),
        ..TaskToolContext::default()
    }
}

fn momus_params(prompt: &str) -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: Some(prompt.to_string()),
        subagent_type: Some("momus".to_string()),
        run_in_background: Some(true),
        ..SpawnParamsInput::default()
    }
}

fn send_input(id_or_name: &str, message: &str) -> SendInput {
    SendInput {
        id_or_name: id_or_name.to_string(),
        message: message.to_string(),
        deliver_as: Some(DeliverAs::Steer),
        all_scope: Default::default(),
        caller_session_id: Default::default(),
    }
}

#[test]
fn given_an_open_plan_session_when_the_full_momus_lifecycle_is_driven_then_forcing_refusal_cancel_and_respawn_all_hold()
 {
    // given the real manager + store, and a session where plan A was referenced 3x and plan B 1x
    let harness = make_manager();
    let manager = &harness.manager;
    let store = &harness.store;
    let in_process = &harness.in_process;

    let tool = TaskToolDeps {
        omo_config: Default::default(),
        agents: Default::default(),
        load_skills: Some(Arc::new(|_: &[String], _: &str| SkillResolution::default())),
        resolve_ancestry: Default::default(),
    };
    let policy = OpenSessionPolicy;
    let execute = build_task_execute(
        TaskExecuteDeps {
            manager,
            tool: &tool,
            policy: &policy,
        },
        ForegroundWaitOptions::default(),
    );
    let context = ctx();

    // when step 1: momus is spawned with a chatty zero-path prompt
    let first = execute
        .execute(
            "call-1",
            &momus_params(
                "please review my plan, I worked really hard on it and think it is great",
            ),
            None,
            None,
            &context,
        )
        .expect("first execute");

    // then the child start spec carries ONLY the canonical contract for the most-referenced plan
    let first_id = first.details.task_id.clone();
    assert!(first_id.starts_with("st_"), "unexpected id {first_id}");
    in_process.wait_handle(&first_id);
    assert_eq!(
        in_process.specs().first().map(|spec| spec.prompt.clone()),
        Some(canonical(PLAN_A))
    );
    let first_record = store.load(&first_id).expect("load").expect("record");
    assert_eq!(first_record.agent_type.as_deref(), Some("momus"));

    // when step 2: momus is spawned with an explicit B path
    let second = execute
        .execute(
            "call-2",
            &momus_params(&format!("review {PLAN_B} please")),
            None,
            None,
            &context,
        )
        .expect("second execute");

    // then the explicit path wins over the most-referenced heuristic
    let second_id = second.details.task_id.clone();
    in_process.wait_handle(&second_id);
    assert_eq!(
        in_process.specs().get(1).map(|spec| spec.prompt.clone()),
        Some(canonical(PLAN_B))
    );

    // when step 3a: the running momus is sent a message
    let reminder = interaction_policy_for_agent("momus")
        .expect("momus policy")
        .send_denial_reminder;
    let running_send = manager
        .send_to_task(&send_input(&first_id, "please reconsider your verdict"))
        .expect("send to running momus");

    // then the refusal is the registry reminder and NOTHING reaches the child session
    match &running_send {
        SendOutcome::OneShotAgent { message, .. } => assert_eq!(message.as_str(), reminder),
        other => panic!("expected one_shot_agent, got {other:?}"),
    }
    let steer_count = |task_id: &str| {
        in_process
            .handle(task_id)
            .map(|handle| handle.steer_calls().len())
            .unwrap_or(0)
    };
    assert_eq!(steer_count(&first_id), 0);

    // when step 3b: the momus child completes and is sent another message
    in_process.wait_handle(&first_id).complete("[OKAY]");
    manager
        .wait_for(&first_id, None, Some(WAIT))
        .unwrap_or_else(|error| panic!("{first_id} never settled: {error}"));
    let completed_send = manager
        .send_to_task(&send_input(&first_id, "one more thing"))
        .expect("send to completed momus");

    // then a finished momus is equally unmessageable - revival is impossible
    assert!(
        matches!(completed_send, SendOutcome::OneShotAgent { .. }),
        "expected one_shot_agent, got {completed_send:?}"
    );
    assert_eq!(steer_count(&first_id), 0);

    // when step 4: the second momus is cancelled
    let cancel = manager.cancel_task(&second_id, Some("no longer needed"), Default::default());

    // then cancel succeeds - create/cancel/output are the only verbs and cancel is one of them
    assert!(
        !format!("{cancel:?}").contains("NotFound"),
        "cancel reported not_found: {cancel:?}"
    );

    // when step 5: a fresh momus is spawned after the cancel
    let third = execute
        .execute(
            "call-3",
            &momus_params(&format!("review {PLAN_A} again")),
            None,
            None,
            &context,
        )
        .expect("third execute");

    // then a new one-shot session is admitted normally
    let third_id = third.details.task_id.clone();
    assert!(third_id.starts_with("st_"));
    in_process.wait_handle(&third_id);
    assert_eq!(
        in_process.specs().get(2).map(|spec| spec.prompt.clone()),
        Some(canonical(PLAN_A))
    );
}
