//! ISO-01 registered native sandbox consumer proof. SOURCE ONLY: nothing here has been compiled or
//! executed; the assembly owner runs the commands in the receipt.
//!
//! The engine is the PRODUCTION composition: `compose_task_engine` builds the real
//! `CoreIsolationRuntime` over `maho-isolation-core` (no fake runtime is injected anywhere), and
//! `TaskComponent::register` exposes the actual `task` / `task_cancel` tools, which this harness
//! invokes. The only controlled seam is the child runner: it records the managed start spec's cwd
//! and writes one fixture file inside that cwd, so the edit lands in the real disposable Git
//! checkout the production runtime prepared, and it reports a deterministic outcome.
//!
//! Run with an isolated HOME: the sandbox root is `<HOME>/.omo/wt`, and the example fails closed if
//! that root already holds an unrelated sandbox, so it can never touch a developer's real clone.

#[path = "../tests/support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use maho_ext_api::*;
use maho_omo_task::component::TaskComponent;
use maho_omo_task::engine::{ComposeTaskEngineDeps, TaskEngine, compose_task_engine};
use senpi_task::isolation::runtime::{
    BackendKind, IsolationOwner, IsolationRuntimeOptions, create_isolation_runtime,
};
use senpi_task::manager::types::{
    ListScope, ManagedRunner, ManagedRunnerError, ManagedRunnerResult, ManagedRunners,
    ManagedStartSpec,
};
use senpi_task::manager::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use senpi_task::runners::RunnerOutcome;
use senpi_task::state::{
    IsolationMergeKind, IsolationRecord, ResidencyState, TaskIsolationSpec, TaskRecord,
    TaskRecordInput, TaskStatus, create_task_record,
};
use senpi_task::store::{StateDirConfig, TaskRecordStore};
use senpi_task::team::liveness_ownership::TeamMemberOwnershipDeps;
use senpi_task::team::runtime_config::TeamTaskBounds;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(30);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn run_git(dir: &Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "isolation proof")
        .env("GIT_AUTHOR_EMAIL", "proof@example.invalid")
        .env("GIT_COMMITTER_NAME", "isolation proof")
        .env("GIT_COMMITTER_EMAIL", "proof@example.invalid")
        .output()?;
    if !output.status.success() {
        return Err(format!("git {args:?} failed: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(())
}

fn git_fixture(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo)?;
    run_git(&repo, &["init", "-q", "-b", "main"])?;
    std::fs::write(repo.join("seed.txt"), "base\n")?;
    run_git(&repo, &["add", "seed.txt"])?;
    run_git(&repo, &["commit", "-q", "-m", "base"])?;
    Ok(repo)
}

/// A pid that is provably gone: a child reaped before the marker is written.
fn dead_pid() -> Result<u32, Box<dyn std::error::Error>> {
    let mut child = Command::new("sh").arg("-c").arg("exit 0").spawn()?;
    let pid = child.id();
    let _ = child.wait()?;
    Ok(pid)
}

struct Child {
    id: String,
    outcome: Mutex<Option<RunnerOutcome>>,
    ready: Condvar,
    listeners: Arc<Mutex<BTreeMap<u64, ManagedChildListener>>>,
}

impl Child {
    fn settle(&self, outcome: RunnerOutcome) {
        *lock(&self.outcome) = Some(outcome);
        self.ready.notify_all();
    }
}

impl ManagedChildHandle for Child {
    fn task_id(&self) -> &str {
        &self.id
    }
    fn session_id(&self) -> Option<String> {
        Some("isolation-proof-child".into())
    }
    fn pid(&self) -> Option<i64> {
        None
    }
    fn steer(&self, _: &str) -> Result<(), senpi_task::host::HostError> {
        Ok(())
    }
    fn follow_up(&self, _: &str) -> Result<(), senpi_task::host::HostError> {
        Ok(())
    }
    fn abort(&self) -> Result<(), senpi_task::host::HostError> {
        self.settle(RunnerOutcome::Cancelled);
        Ok(())
    }
    fn subscribe(&self, listener: ManagedChildListener) -> Unsubscribe {
        let mut listeners = lock(&self.listeners);
        let id = listeners.keys().next_back().copied().unwrap_or(0) + 1;
        listeners.insert(id, listener);
        drop(listeners);
        let listeners = self.listeners.clone();
        Box::new(move || {
            lock(&listeners).remove(&id);
        })
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        let guard = lock(&self.outcome);
        let (mut guard, timeout) = self
            .ready
            .wait_timeout_while(guard, WAIT, |outcome| outcome.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        assert!(!timeout.timed_out(), "child outcome was never signaled");
        guard.take().unwrap_or(RunnerOutcome::Cancelled)
    }
    fn last_assistant_text(&self) -> Option<String> {
        None
    }
    fn dispose(&self) -> Result<(), senpi_task::host::HostError> {
        lock(&self.listeners).clear();
        Ok(())
    }
}

/// The controlled child runner: it records the managed start spec and mutates the REAL sandbox at
/// `spec.cwd`, then hands back a handle the harness settles.
#[derive(Default)]
struct RecordingRunner {
    starts: Mutex<Vec<ManagedStartSpec>>,
    child: Mutex<Option<Arc<Child>>>,
    write: Mutex<Option<(String, String)>>,
}

impl RecordingRunner {
    fn arm_write(&self, relative: &str, content: &str) {
        *lock(&self.write) = Some((relative.to_string(), content.to_string()));
    }

    fn start_count(&self) -> usize {
        lock(&self.starts).len()
    }

    fn last_cwd(&self) -> Option<String> {
        lock(&self.starts).last().map(|spec| spec.cwd.clone())
    }

    fn child(&self) -> Option<Arc<Child>> {
        lock(&self.child).clone()
    }
}

impl ManagedRunner for RecordingRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        lock(&self.starts).push(spec.clone());
        if let Some((relative, content)) = lock(&self.write).clone() {
            let target = Path::new(&spec.cwd).join(&relative);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    ManagedRunnerError::Other(format!("sandbox write dir failed: {error}"))
                })?;
            }
            std::fs::write(&target, content).map_err(|error| {
                ManagedRunnerError::Other(format!(
                    "controlled child could not write {} inside the managed cwd: {error}",
                    target.display()
                ))
            })?;
        }
        let child = Arc::new(Child {
            id: spec.task_id.clone(),
            outcome: Mutex::new(None),
            ready: Condvar::new(),
            listeners: Arc::default(),
        });
        *lock(&self.child) = Some(child.clone());
        Ok(child)
    }
}

struct Actions {
    messages: Mutex<usize>,
}

impl ExtensionActions for Actions {
    fn send_message(&self, _: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> {
        *lock(&self.messages) += 1;
        Ok(())
    }
    fn send_user_message(
        &self,
        _: UserMessageContent,
        _: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new("unexpected user message"))
    }
    fn append_entry(&self, _: &str, _: Option<Value>) -> Result<(), ExtensionFailure> {
        Err(ExtensionFailure::new("unexpected entry"))
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(vec![])
    }
}

fn compose(
    cwd: &Path,
    state_dir: &Path,
    runner: Arc<RecordingRunner>,
    actions: Arc<Actions>,
) -> TaskEngine {
    compose_task_engine(ComposeTaskEngineDeps {
        cwd: cwd.to_path_buf(),
        config: json!({
            "task": {
                "state_dir": state_dir.to_string_lossy(),
                "isolation": { "enabled": true, "backend": "rcopy", "apply": true, "merge": "patch" }
            }
        }),
        runners: ManagedRunners {
            in_process: runner.clone(),
            process: runner,
        },
        actions,
        coordinator: None,
        resolve_registry: Arc::new(|| None),
        host_transport: None,
    })
}

fn register(
    api: &mut ExtensionApi,
    engine: TaskEngine,
    root: &Path,
) -> Result<Arc<TaskComponent>, Box<dyn std::error::Error>> {
    let ownership = TeamMemberOwnershipDeps {
        state_dir: StateDirConfig {
            project_dir: root.to_path_buf(),
            task_state_dir: None,
        },
        team_bounds: TeamTaskBounds {
            max_members: 4,
            max_parallel_members: 2,
            max_wall_clock_minutes: 10,
        },
        load_runtime_state: None,
    };
    TaskComponent::register(api, engine, Default::default(), ownership, false)?
        .ok_or_else(|| "task component was disabled".into())
}

fn tool_definition(
    api: &ExtensionApi,
    name: &str,
) -> Result<ToolDefinition, Box<dyn std::error::Error>> {
    Ok(api
        .registered
        .tools
        .iter()
        .find(|tool| tool.definition.name == name)
        .ok_or_else(|| format!("registered tool {name} is missing"))?
        .definition
        .clone())
}

async fn spawn_isolated(
    tool: &ToolDefinition,
    call_id: &str,
    prompt: &str,
    apply: bool,
    context: &ExtensionContext,
) -> Result<Value, Box<dyn std::error::Error>> {
    let result = (tool.execute)(ToolCall {
        id: call_id,
        params: json!({
            "prompt": prompt,
            "subagent_type": "explore",
            "model": "faux/native",
            "isolated": true,
            "apply": apply,
            "merge": "patch",
            "run_in_background": true
        }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(context),
    })
    .await?;
    result.details.ok_or_else(|| "task result carried no details".into())
}

async fn dispatch(
    api: &ExtensionApi,
    kind: EventKind,
    event: &mut ExtensionEvent,
    context: &ExtensionContext,
) -> Result<(), ExtensionFailure> {
    for handler in api.registered.handlers.get(&kind).into_iter().flatten() {
        handler(event, context).await?;
    }
    Ok(())
}

fn isolation_of(
    record: &TaskRecord,
) -> Result<&IsolationRecord, Box<dyn std::error::Error>> {
    record
        .isolation
        .as_ref()
        .ok_or_else(|| "record carries no isolation".into())
}

fn merge_of(
    record: &TaskRecord,
) -> Result<&senpi_task::state::IsolationMergeResult, Box<dyn std::error::Error>> {
    isolation_of(record)?
        .merge_result
        .as_ref()
        .ok_or_else(|| "record carries no isolation merge_result".into())
}

/// Waits (bounded, event-driven by store mutations) for a record to carry a merge result. Used only
/// by the cancellation scenario, where the cancel writes the terminal record before the outcome
/// watcher settles the isolation.
fn wait_for_merge_result(
    store: &TaskRecordStore,
    task_id: &str,
    signal: &Arc<(Mutex<()>, Condvar)>,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + WAIT;
    let (guard, wake) = &**signal;
    let mut guard = lock(guard);
    loop {
        let settled = store
            .load(task_id)?
            .and_then(|record| record.isolation)
            .and_then(|isolation| isolation.merge_result)
            .is_some();
        if settled {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "the cancelled run's isolation settle never landed");
        guard = wake
            .wait_timeout(guard, remaining)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

fn retained_sibling(base_dir: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let parent = base_dir.parent().ok_or("sandbox base dir has no parent")?;
    let prefix = format!(
        "{}.retained-",
        base_dir.file_name().ok_or("sandbox base dir has no name")?.to_string_lossy()
    );
    let mut found: Vec<PathBuf> = std::fs::read_dir(parent)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .next()
        .ok_or_else(|| format!("no retained sandbox beside {}", base_dir.display()).into())
}

#[tokio::main(flavor = "current_thread")]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let home = std::env::var_os("HOME").ok_or("HOME must be set; run this example with an isolated HOME")?;
    let home = PathBuf::from(home);
    let sandbox_root = home.join(".omo").join("wt");
    if let Ok(entries) = std::fs::read_dir(&sandbox_root)
        && entries.filter_map(Result::ok).next().is_some()
    {
        return Err(format!(
            "refusing to run: {} already holds a sandbox; use an isolated HOME",
            sandbox_root.display()
        )
        .into());
    }

    let root = tempfile::tempdir()?;
    let repo = git_fixture(root.path())?;
    let state_dir = root.path().join("state");
    std::fs::create_dir_all(&state_dir)?;
    let actions = Arc::new(Actions { messages: Mutex::new(0) });
    let runner = Arc::new(RecordingRunner::default());

    let engine = compose(&repo, &state_dir, runner.clone(), actions.clone());
    let mut api = support::api();
    api.runtime.bind(actions.clone());
    let component = register(&mut api, engine, root.path())?;
    let mut context = support::context();
    context.cwd = repo.clone();
    let task_tool = tool_definition(&api, "task")?;
    let cancel_tool = tool_definition(&api, "task_cancel")?;
    let manager = component.engine.manager.clone();

    // 1. explicit rcopy / patch / apply=true.
    runner.arm_write("a.txt", "from the child\n");
    let started = spawn_isolated(&task_tool, "iso-apply-true", "write a.txt", true, &context).await?;
    let task_id = started["task_id"].as_str().ok_or("scenario 1 start has no task_id")?.to_string();
    assert_eq!(started["status"], "running");
    let record = manager.get(&task_id).ok_or("scenario 1 record missing")?;
    let spec = &isolation_of(&record)?.spec;
    assert_eq!(spec.backend, BackendKind::Rcopy);
    assert_eq!(spec.fell_back, Some(false));
    assert!(spec.apply);
    let base_dir = PathBuf::from(&spec.base_dir);
    let merged_dir = PathBuf::from(&spec.merged_dir);
    assert!(base_dir.starts_with(&sandbox_root));
    assert_ne!(merged_dir, repo);
    assert!(merged_dir.join(".git").exists());
    assert!(merged_dir.join("seed.txt").exists());
    assert_eq!(runner.start_count(), 1);
    assert_eq!(runner.last_cwd().as_deref(), Some(spec.merged_dir.as_str()));
    let spawn = record.spawn_spec.as_ref().and_then(|spawn| spawn.as_v1()).ok_or("spawn spec is not V1")?;
    assert_eq!(spawn.cwd, spec.merged_dir);
    assert!(spawn.isolation.is_some());
    assert!(!repo.join("a.txt").exists());
    runner.child().ok_or("scenario 1 child missing")?.settle(RunnerOutcome::completed("applied"));
    let record = manager.wait_for(&task_id, None, Some(WAIT))?;
    assert_eq!(record.status, TaskStatus::Completed);
    assert_eq!(std::fs::read_to_string(repo.join("a.txt"))?, "from the child\n");
    let merge = merge_of(&record)?;
    assert_eq!(merge.kind, IsolationMergeKind::Applied);
    assert!(merge.changes_applied);
    assert!(merge.duration_ms.is_some());
    assert!(PathBuf::from(merge.patch_path.as_ref().ok_or("no patch path")?).exists());
    assert!(!base_dir.exists(), "an applied merge tears the sandbox down");
    manager.forget(&task_id);
    println!("PASS isolation apply=true patch merges the child's write into the parent checkout and cleans the sandbox");

    // 2. apply=false.
    runner.arm_write("b.txt", "never merged\n");
    let started = spawn_isolated(&task_tool, "iso-apply-false", "write b.txt", false, &context).await?;
    let task_id = started["task_id"].as_str().ok_or("scenario 2 start has no task_id")?.to_string();
    let record = manager.get(&task_id).ok_or("scenario 2 record missing")?;
    let base_dir = PathBuf::from(&isolation_of(&record)?.spec.base_dir);
    assert!(!isolation_of(&record)?.spec.apply);
    runner.child().ok_or("scenario 2 child missing")?.settle(RunnerOutcome::completed("done"));
    let record = manager.wait_for(&task_id, None, Some(WAIT))?;
    assert!(!repo.join("b.txt").exists());
    let merge = merge_of(&record)?;
    assert_eq!(merge.kind, IsolationMergeKind::Retained);
    assert!(!merge.changes_applied);
    assert!(PathBuf::from(merge.patch_path.as_ref().ok_or("no patch path")?).exists());
    assert!(PathBuf::from(merge.summary_path.as_ref().ok_or("no summary path")?).exists());
    // `Retained` is not a retaining kind in isolation/settle.rs::dispose_workspace, so apply=false
    // writes its artifacts and then disposes the sandbox through the recorded backend.
    assert!(!base_dir.exists());
    manager.forget(&task_id);
    println!("PASS isolation apply=false retains artifacts without replaying into the parent");

    // 3a. runner-reported cancellation.
    runner.arm_write("c.txt", "abandoned\n");
    let started = spawn_isolated(&task_tool, "iso-cancelled", "write c.txt", true, &context).await?;
    let task_id = started["task_id"].as_str().ok_or("scenario 3a start has no task_id")?.to_string();
    let record = manager.get(&task_id).ok_or("scenario 3a record missing")?;
    let base_dir = PathBuf::from(&isolation_of(&record)?.spec.base_dir);
    runner.child().ok_or("scenario 3a child missing")?.settle(RunnerOutcome::Cancelled);
    let record = manager.wait_for(&task_id, None, Some(WAIT))?;
    assert_eq!(record.status, TaskStatus::Cancelled);
    assert!(!repo.join("c.txt").exists());
    let merge = merge_of(&record)?;
    assert_eq!(merge.kind, IsolationMergeKind::Retained);
    assert!(!merge.changes_applied);
    assert!(PathBuf::from(merge.patch_path.as_ref().ok_or("no patch path")?).exists());
    assert!(!base_dir.exists());
    manager.forget(&task_id);
    println!("PASS isolated child cancellation retains the delta and never auto-merges");

    // 3b. the ACTUAL registered task_cancel tool. The pending-stop settlement (see the receipt)
    // makes the merge result land deterministically; a leaked sandbox must fail the run, so no
    // hand deletion is performed.
    runner.arm_write("d.txt", "abandoned\n");
    let started = spawn_isolated(&task_tool, "iso-tool-cancel", "write d.txt", true, &context).await?;
    let task_id = started["task_id"].as_str().ok_or("scenario 3b start has no task_id")?.to_string();
    let record = manager.get(&task_id).ok_or("scenario 3b record missing")?;
    let base_dir = PathBuf::from(&isolation_of(&record)?.spec.base_dir);
    let settle_signal = Arc::new((Mutex::new(()), Condvar::new()));
    let notifier = settle_signal.clone();
    // The callback and the waiter below share this mutex, so a store write landing between the
    // predicate read and the wait can never be lost.
    component.engine.store.set_mutation_listener(Some(Arc::new(move || {
        let _guard = lock(&notifier.0);
        notifier.1.notify_all();
    })));
    let cancelled = (cancel_tool.execute)(ToolCall {
        id: "iso-tool-cancel-call",
        params: json!({ "task_id": task_id, "reason": "isolation proof cancellation" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: Some(&context),
    })
    .await?;
    assert_eq!(cancelled.details.as_ref().and_then(|details| details["kind"].as_str()), Some("cancelled"));
    let record = manager.wait_for(&task_id, None, Some(WAIT))?;
    assert_eq!(record.status, TaskStatus::Cancelled);
    wait_for_merge_result(&component.engine.store, &task_id, &settle_signal)?;
    component.engine.store.set_mutation_listener(None);
    let record = component.engine.store.load(&task_id)?.ok_or("scenario 3b record vanished")?;
    let merge = merge_of(&record)?;
    assert_eq!(merge.kind, IsolationMergeKind::Retained, "a cancel never merges");
    assert!(!merge.changes_applied);
    assert!(PathBuf::from(merge.patch_path.as_ref().ok_or("cancelled merge has no patch path")?).exists());
    assert!(!repo.join("d.txt").exists());
    assert!(!base_dir.exists(), "the cancelled sandbox must be disposed, not leaked");
    manager.forget(&task_id);
    println!("PASS registered task_cancel settles the isolated child as retained without merging and cleans the sandbox");

    // 4. divergent parent.
    runner.arm_write("seed.txt", "child edit\n");
    let started = spawn_isolated(&task_tool, "iso-conflict", "edit seed.txt", true, &context).await?;
    let task_id = started["task_id"].as_str().ok_or("scenario 4 start has no task_id")?.to_string();
    let record = manager.get(&task_id).ok_or("scenario 4 record missing")?;
    let base_dir = PathBuf::from(&isolation_of(&record)?.spec.base_dir);
    std::fs::write(repo.join("seed.txt"), "parent edit\n")?;
    runner.child().ok_or("scenario 4 child missing")?.settle(RunnerOutcome::completed("divergent"));
    let record = manager.wait_for(&task_id, None, Some(WAIT))?;
    let merge = merge_of(&record)?;
    assert_eq!(merge.kind, IsolationMergeKind::NotApplied);
    assert!(!merge.changes_applied);
    assert!(merge.conflict.as_deref().is_some_and(|conflict| !conflict.is_empty()));
    assert!(merge.manual_command.as_deref().is_some_and(|command| command.starts_with("git apply --3way")));
    assert_eq!(std::fs::read_to_string(repo.join("seed.txt"))?, "parent edit\n");
    assert!(PathBuf::from(merge.patch_path.as_ref().ok_or("no patch path")?).exists());
    assert!(!base_dir.exists());
    let retained = retained_sibling(&base_dir)?;
    assert!(retained.exists());
    manager.forget(&task_id);
    std::fs::remove_dir_all(&retained)?;
    println!("PASS divergent parent conflict retains the sandbox with conflict and manual-command evidence");

    // 5. non-Git cwd.
    let non_git = root.path().join("not-a-repo");
    std::fs::create_dir_all(&non_git)?;
    let state_dir = root.path().join("state-nongit");
    std::fs::create_dir_all(&state_dir)?;
    let actions = Arc::new(Actions { messages: Mutex::new(0) });
    let runner = Arc::new(RecordingRunner::default());
    let engine = compose(&non_git, &state_dir, runner.clone(), actions.clone());
    let mut api_b = support::api();
    api_b.runtime.bind(actions.clone());
    let component_b = register(&mut api_b, engine, root.path())?;
    let mut context_b = support::context();
    context_b.cwd = non_git.clone();
    let task_tool_b = tool_definition(&api_b, "task")?;
    let manager_b = component_b.engine.manager.clone();
    let refused = spawn_isolated(&task_tool_b, "iso-non-git", "never launches", true, &context_b).await?;
    assert_eq!(refused["status"], "error");
    let reason = refused["reason"].as_str().ok_or("refusal has no reason")?;
    assert!(reason.starts_with("isolation_unavailable:"), "{reason}");
    assert!(reason.contains("not a git checkout"), "{reason}");
    assert_eq!(runner.start_count(), 0);
    assert_eq!(std::fs::read_dir(&non_git)?.count(), 0);
    let refused_id = refused["task_id"].as_str().ok_or("refusal has no task_id")?;
    let refused_record = manager_b.get(refused_id).ok_or("refusal record missing")?;
    assert_eq!(refused_record.status, TaskStatus::Error);
    assert!(refused_record.isolation.is_none());
    assert!(manager_b.list(&ListScope::All).iter().all(|entry| entry.record.status.is_terminal()));
    println!("PASS non-git cwd refuses the isolated launch without starting the runner");

    // 6. session-start recovery: a persisted terminal isolation without a merge result is salvaged
    //    (merge=false, reason host_crashed) and only a provably dead owner's sandbox is reclaimed.
    //    The dead/live owner markers are built from the production marker format: the hostname comes
    //    from a marker the production runtime wrote, and only the pid is replaced with a dead one.
    let marker_runtime = create_isolation_runtime(&IsolationRuntimeOptions::default());
    let live_base = sandbox_root.join("t00face0001");
    std::fs::create_dir_all(&live_base)?;
    marker_runtime.write_owner(&live_base, "st_00face01", &IsolationOwner::default());
    let marker_file = std::fs::read_dir(&live_base)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.is_file())
        .ok_or("production owner marker missing")?;
    let marker: Value = serde_json::from_str(&std::fs::read_to_string(&marker_file)?)?;
    let hostname = marker["hostname"].as_str().ok_or("marker has no hostname")?.to_string();

    let dead_base = sandbox_root.join("t00dead0001");
    std::fs::create_dir_all(dead_base.join("m"))?;
    std::fs::write(dead_base.join(".omo-isolation-backend.json"), json!({ "backend": "rcopy" }).to_string())?;
    std::fs::write(
        dead_base.join(".omo-isolation-owner.json"),
        json!({
            "id": "st_00dead01",
            "hostname": hostname,
            "created_at": 0,
            "host": { "pid": dead_pid()?, "start_identity": null }
        })
        .to_string(),
    )?;

    let salvage_base = sandbox_root.join("t00cafe0001");
    let salvage_input = TaskRecordInput {
        parent_session_id: "salvaged-session".to_string(),
        root_session_id: "salvaged-session".to_string(),
        depth: 1,
        execution_mode: "in-process".to_string(),
        model: "faux/native".to_string(),
        ..TaskRecordInput::default()
    };
    let salvage_record = TaskRecord {
        status: TaskStatus::Completed,
        residency_state: ResidencyState::PersistedOnly,
        isolation: Some(IsolationRecord::new(TaskIsolationSpec {
            backend: BackendKind::Rcopy,
            fell_back: Some(false),
            merged_dir: salvage_base.join("m").to_string_lossy().into_owned(),
            base_dir: salvage_base.to_string_lossy().into_owned(),
            mode: senpi_task::state::IsolationMergeMode::Patch,
            apply: true,
        })),
        ..create_task_record(salvage_input, Some(0))?
    };
    let salvage_id = salvage_record.task_id.clone();
    component.engine.store.save(&salvage_record)?;

    let parent_before = std::fs::read_to_string(repo.join("seed.txt"))?;
    let entries_before = std::fs::read_dir(&repo)?.count();
    dispatch(
        &api,
        EventKind::SessionStart,
        &mut ExtensionEvent::SessionStart(SessionStartEvent {
            reason: SessionReason::New,
            initial_model_provenance: None,
            previous_session_file: None,
        }),
        &context,
    )
    .await?;

    let salvaged = component.engine.store.load(&salvage_id)?.ok_or("salvaged record vanished")?;
    let salvage_merge = merge_of(&salvaged)?;
    assert_eq!(salvage_merge.kind, IsolationMergeKind::Retained, "a crash salvage never merges");
    assert!(!salvage_merge.changes_applied);
    assert_eq!(salvage_merge.reason.as_deref(), Some("host_crashed"));
    assert_eq!(std::fs::read_to_string(repo.join("seed.txt"))?, parent_before, "the parent must be untouched by salvage");
    assert_eq!(std::fs::read_dir(&repo)?.count(), entries_before, "the parent must gain no file from salvage");
    assert!(!dead_base.exists(), "a provably dead owner's sandbox must be reclaimed");
    assert!(live_base.exists(), "a live owner's sandbox must be kept");
    std::fs::remove_dir_all(&live_base)?;
    println!("PASS session-start salvage retains a crashed isolation and reclaims only a dead owner's sandbox");

    let leftovers: Vec<PathBuf> = match std::fs::read_dir(&sandbox_root) {
        Ok(entries) => entries.filter_map(Result::ok).map(|entry| entry.path()).collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    assert!(leftovers.is_empty(), "every sandbox must be gone: {leftovers:?}");
    component_b.dispose();
    component.dispose();
    drop(component_b);
    drop(component);
    drop(api_b);
    drop(api);
    root.close()?;
    println!("cleanup: every sandbox removed, components disposed, temporary state removed");
    Ok(())
}
