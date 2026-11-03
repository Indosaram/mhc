//! Member ancestry (docs/cleanroom/specs/team.md "Delta (omo v5.1.52)" D1, tests 1-3): members
//! record the lead as owner and the tree root as root, their children run one level below the
//! lead, and follow-up runs carry the same parent, root and task id.
use maho_lsp::hooks::HookChain;
use maho_task::runner::BoxFuture;
use maho_task::settings::{DEPTH_ENV, PARENT_SESSION_ENV, ROOT_SESSION_ENV, TASK_ID_ENV};
use maho_task::{
    CancelToken, Lineage, RunOutcome, RunSpec, TaskAddon, TaskExtension, TaskRunner, TaskScope,
    TaskStatus,
};
use maho_team::TeamAddon;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(10);

/// Finishes every run at once and records its spec.
#[derive(Default)]
struct Recording {
    specs: Mutex<Vec<RunSpec>>,
}

impl TaskRunner for Recording {
    fn run<'a>(
        &'a self,
        spec: &'a RunSpec,
        _cancel: CancelToken,
        on_start: Box<dyn FnOnce(Option<u32>) + Send + 'a>,
    ) -> BoxFuture<'a, RunOutcome> {
        Box::pin(async move {
            on_start(None);
            self.specs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(spec.clone());
            std::fs::write(&spec.output_path, "done").expect("output");
            RunOutcome {
                exit_code: Some(0),
                ..RunOutcome::default()
            }
        })
    }
}

fn env_of(spec: &RunSpec) -> Vec<(&'static str, String)> {
    spec.child_env()
}

async fn call(scope: &TaskScope, name: &str, params: Value) -> maho_ext_api::ToolResult {
    let tool = TeamAddon::default()
        .tools()
        .into_iter()
        .find(|tool| tool.name == name)
        .expect("tool");
    (tool.handler)(scope.clone(), params, CancelToken::default())
        .await
        .expect("tool result")
}

fn scope(
    dir: &std::path::Path,
    runner: &Arc<Recording>,
    lead: &str,
    lineage: Lineage,
    max_depth: u32,
) -> TaskScope {
    let extension = TaskExtension {
        agent_dir: Some(dir.join("agent")),
        runner: Arc::clone(runner) as Arc<dyn TaskRunner>,
        available_models: Arc::new(Vec::<String>::new),
        isolation: None,
        observers: Vec::new(),
        hooks: HookChain::default(),
        addons: vec![Arc::new(TeamAddon::default()) as Arc<dyn TaskAddon>],
    };
    TaskScope::detached_in(
        extension,
        dir.to_path_buf(),
        lead,
        json!({"task": {"max_depth": max_depth}}),
        lineage,
    )
}

async fn settle(scope: &TaskScope, id: &str, runs: u32) {
    let record = scope.manager().wait(id, Some(LIMIT)).await.expect("record");
    assert_eq!(
        (record.status, record.runs),
        (TaskStatus::Completed, runs),
        "{record:?}"
    );
}

/// Tests 1 and 3: a top-level lead.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn top_level_lead_members_and_follow_ups() {
    let dir = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Recording::default());
    let scope = scope(dir.path(), &runner, "L", Lineage::default(), 1);
    let created = call(
        &scope,
        "team_create",
        json!({"name": "t", "members": [{"name": "a", "task": "work a"}]}),
    )
    .await;
    assert_ne!(created.is_error, Some(true));
    settle(&scope, "t-a", 1).await;
    let record = scope.manager().get("t-a").expect("record");
    assert_eq!(record.owner_session.as_deref(), Some("L"));
    assert_eq!(record.root_session.as_deref(), Some("L"));
    let first = runner.specs.lock().expect("specs")[0].clone();
    assert_eq!(first.depth, 1);
    let first_env = env_of(&first);
    for (key, value) in [
        (DEPTH_ENV, "1"),
        (TASK_ID_ENV, "t-a"),
        (ROOT_SESSION_ENV, "L"),
        (PARENT_SESSION_ENV, "L"),
    ] {
        assert!(
            first_env.contains(&(key, value.to_owned())),
            "{key}: {first_env:?}"
        );
    }

    // Test 3: a message to the idle member starts a run with the same ancestry.
    call(
        &scope,
        "team_message",
        json!({"team": "t", "member": "a", "message": "one more"}),
    )
    .await;
    settle(&scope, "t-a", 2).await;
    let second = runner.specs.lock().expect("specs")[1].clone();
    assert_eq!(second.prompt, "one more");
    assert_eq!(env_of(&second), first_env);
    assert_eq!(second.session_id, first.session_id);
}

/// Test 2: a lead that is itself a delegated child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nested_lead_members_record_the_tree_root() {
    let dir = tempfile::tempdir().expect("tmp");
    let runner = Arc::new(Recording::default());
    let lineage = Lineage {
        depth: 1,
        root_session: Some("R".into()),
    };
    let scope = scope(dir.path(), &runner, "L", lineage, 3);
    call(
        &scope,
        "team_create",
        json!({"name": "n", "members": [{"name": "a", "task": "work a"}, {"name": "b", "task": "work b"}]}),
    )
    .await;
    for id in ["n-a", "n-b"] {
        settle(&scope, id, 1).await;
        let record = scope.manager().get(id).expect("record");
        assert_eq!(record.owner_session.as_deref(), Some("L"));
        assert_eq!(record.root_session.as_deref(), Some("R"));
    }
    let specs = runner.specs.lock().expect("specs").clone();
    assert_eq!(specs.len(), 2);
    for spec in specs {
        assert_eq!(spec.depth, 2);
        let env = env_of(&spec);
        assert!(env.contains(&(ROOT_SESSION_ENV, "R".to_owned())), "{env:?}");
        assert!(
            env.contains(&(PARENT_SESSION_ENV, "L".to_owned())),
            "{env:?}"
        );
        assert!(env.contains(&(DEPTH_ENV, "2".to_owned())), "{env:?}");
    }
}
