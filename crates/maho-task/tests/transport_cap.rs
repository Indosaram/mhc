//! Integration tests for task completion transport cap and wake batch limiting
//! (`docs/cleanroom/specs/ultrawork.md` §9, GAP-5).
use maho_ext_api::{
    CustomMessage, ExtensionActions, ExtensionApi, ExtensionFailure, ExtensionRuntime,
    ExtensionSessionProfile, LoadedExtension, SendMessageOptions, SendUserMessageOptions,
    ToolContent, ToolInfo, UserMessageContent,
};
use maho_task::runner::{BoxFuture, CancelToken, RunOutcome, RunSpec, TaskRunner};
use maho_task::transport::{
    ChildCompletion, CompletionCapOptions, MIN_COMPLETION_BATCH_LIMIT,
    MIN_COMPLETION_TRANSPORT_LIMIT, SettledDelivery, WakeBatchOptions, bind_settled_batcher,
    build_capped_result_envelope, build_wake_batch, cap_completion_response,
    count_utf16_code_units, deliver_batch, deliver_settled, read_overflow_manifest,
};
use maho_task::{
    ModelChoice, SpawnRequest, StateDir, TaskManager, TaskManagerOptions, TaskRecord,
    TaskSettings, TaskStatus, bundled_agents,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tempfile::tempdir;

#[derive(Default)]
struct MessageSink {
    messages: Mutex<Vec<CustomMessage>>,
}

impl ExtensionActions for MessageSink {
    fn send_message(&self, msg: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> {
        self.messages.lock().unwrap().push(msg);
        Ok(())
    }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { Ok(()) }
    fn append_entry(&self, _: &str, _: Option<serde_json::Value>) -> Result<(), ExtensionFailure> { Ok(()) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(Vec::new()) }
}

fn test_api(sink: Arc<MessageSink>) -> ExtensionApi {
    let runtime = ExtensionRuntime::default();
    runtime.bind(sink);
    ExtensionApi::new(LoadedExtension::new("task", "/tmp".into(), Default::default()), ExtensionSessionProfile::default(), Default::default(), runtime)
}

fn get_content_text(msg: &CustomMessage) -> &str {
    match &msg.content[0] {
        ToolContent::Text { text, .. } => text.as_str(),
        _ => panic!("expected text content in custom message"),
    }
}

fn sample_record(id: &str) -> TaskRecord {
    serde_json::from_value(serde_json::json!({
        "id": id, "agent": "task", "description": "test", "status": "completed",
        "model": "test/model", "thinking": null, "model_source": "parent",
        "session_id": format!("{id}-session"), "cwd": "/", "background": true,
        "pid": null, "exit_code": Some(0), "error": null, "runs": 1,
        "started_at_ms": 1000, "finished_at_ms": 2500,
    }))
    .expect("record")
}

struct FakeRunner {
    release: tokio::sync::Semaphore,
    outputs: Mutex<HashMap<String, String>>,
    logs: Mutex<HashMap<String, String>>,
    failures: Mutex<HashSet<String>>,
}

impl FakeRunner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            release: tokio::sync::Semaphore::new(0),
            outputs: Mutex::default(),
            logs: Mutex::default(),
            failures: Mutex::default(),
        })
    }
}

impl TaskRunner for FakeRunner {
    fn run<'a>(
        &'a self,
        spec: &'a RunSpec,
        cancel: CancelToken,
        on_start: Box<dyn FnOnce(Option<u32>) + Send + 'a>,
    ) -> BoxFuture<'a, RunOutcome> {
        Box::pin(async move {
            on_start(None);
            if spec.prompt.contains("hold") {
                tokio::select! {
                    permit = self.release.acquire() => {
                        if let Ok(p) = permit {
                            p.forget();
                        }
                    }
                    () = cancel.cancelled() => {
                        return RunOutcome { cancelled: true, ..Default::default() };
                    }
                }
            }
            if let Some(out) = self.outputs.lock().unwrap().get(&spec.task_id) {
                let _ = std::fs::write(&spec.output_path, out);
            }
            if let Some(log) = self.logs.lock().unwrap().get(&spec.task_id) {
                let _ = std::fs::write(&spec.log_path, log);
            }
            let is_failure = self.failures.lock().unwrap().contains(&spec.task_id);
            if is_failure {
                RunOutcome {
                    exit_code: Some(1),
                    cancelled: false,
                    error: Some("process failed".to_owned()),
                }
            } else {
                RunOutcome {
                    exit_code: Some(0),
                    cancelled: false,
                    error: None,
                }
            }
        })
    }
}

fn test_manager(dir: &Path, runner: Arc<FakeRunner>) -> Arc<TaskManager> {
    TaskManager::new(TaskManagerOptions {
        state: StateDir::new(dir.join("state")),
        settings: TaskSettings::default(),
        runner: runner as Arc<dyn TaskRunner>,
        isolation: None,
        observers: Vec::new(),
        hooks: maho_lsp::hooks::HookChain::default(),
        depth: 0,
    })
}

fn spawn_request(dir: &Path, id: &str, task: &str) -> SpawnRequest {
    SpawnRequest {
        agent: bundled_agents().remove(0),
        task: task.to_owned(),
        context: None,
        name: Some(id.to_owned()),
        choice: ModelChoice::default(),
        cwd: dir.to_path_buf(),
        background: true,
        isolated: false,
        group: None,
        pane: None,
        quiet: false,
        owner_session: Some("test-session".to_owned()),
        root_session: Some("test-session".to_owned()),
    }
}

/// Spec §9.1 and Test Vectors 37 & 38: Per-child completion transport limit with spill.
#[test]
fn result_capped_and_spilled() {
    let dir = tempdir().expect("tempdir");
    let state_dir = dir.path();

    // 1. Test Vector 37: 10,000 UTF-16 units with limit 8,000.
    let response_10k = "A".repeat(10_000);
    let options = CompletionCapOptions::new(state_dir, "task-1", &response_10k).with_limit(8_000);
    let details = cap_completion_response(options).expect("cap response");

    assert_eq!(details["id"], "task-1");
    assert_eq!(details["status"], "completed");
    assert_eq!(details["spilled"], true);

    let sliced = details["response"].as_str().expect("sliced response");
    assert_eq!(count_utf16_code_units(sliced), 8_000);
    assert_eq!(details["text"], sliced);

    let spilled_file = state_dir.join("completion-results/task-1.txt");
    assert!(spilled_file.is_file(), "spill file must exist");
    let stored_text = std::fs::read_to_string(&spilled_file).expect("read spill");
    assert_eq!(stored_text, response_10k);

    let expected_uri = format!("local://{}", spilled_file.display());
    assert_eq!(details["file_uri"], expected_uri);
    assert_eq!(details["uri"], expected_uri);

    // 2. Test Vector 38: Surrogate pair at boundary (4-byte emoji).
    let mut response_emoji = "b".repeat(7_999);
    response_emoji.push('😀'); // U+1F600, 2 UTF-16 code units
    let options = CompletionCapOptions::new(state_dir, "task-emoji", &response_emoji).with_limit(8_000);
    let details = cap_completion_response(options).expect("cap emoji response");

    let emoji_sliced = details["response"].as_str().expect("emoji sliced");
    assert_eq!(count_utf16_code_units(emoji_sliced), 7_999);
    assert!(!emoji_sliced.contains('😀'));
    let emoji_file = state_dir.join("completion-results/task-emoji.txt");
    assert_eq!(std::fs::read_to_string(emoji_file).expect("read"), response_emoji);

    // 3. Response within limit is delivered inline without spill.
    let short_resp = "short response";
    let options = CompletionCapOptions::new(state_dir, "task-short", short_resp).with_limit(8_000);
    let details = cap_completion_response(options).expect("cap short");
    assert_eq!(details["spilled"], false);
    assert_eq!(details["response"], short_resp);
    assert!(details.get("file_uri").is_none());
    assert!(!state_dir.join("completion-results/task-short.txt").exists());

    // 4. QA failure scenario: Configured limit below minimum (1,000) clamps to minimum.
    let response_1200 = "x".repeat(1_200);
    let options = CompletionCapOptions::new(state_dir, "task-clamp", &response_1200).with_limit(500);
    let details = cap_completion_response(options).expect("cap clamp");
    assert_eq!(details["spilled"], true);
    assert_eq!(count_utf16_code_units(details["response"].as_str().unwrap()), MIN_COMPLETION_TRANSPORT_LIMIT);
}

/// Spec §9.2 and Test Vector 39: Parent wake batch limit with overflow manifest and trailer pointer.
#[test]
fn batch_capped_with_pointers() {
    let dir = tempdir().expect("tempdir");
    let state_dir = dir.path();

    // 1. Test Vector 39: 5 children, each producing 6,000 UTF-16 units, batch limit 24,000.
    let tasks: Vec<ChildCompletion> = (0..5)
        .map(|i| {
            let body = format!("Child {i} payload: {}", "M".repeat(5_980));
            ChildCompletion::new(format!("task-{i}"), "completed", body)
        })
        .collect();

    let options = WakeBatchOptions::new(state_dir)
        .with_batch_id("test-batch-39")
        .with_limit(24_000);

    let res = build_wake_batch(options, &tasks).expect("build batch");

    assert_eq!(res.inline_tasks.len(), 3);
    assert_eq!(res.overflow_tasks.len(), 2);
    assert!(count_utf16_code_units(&res.message) <= 24_000);

    let manifest_path = res.overflow_manifest_path.expect("manifest path");
    assert!(manifest_path.is_file(), "overflow manifest file must exist");
    let manifest = read_overflow_manifest(&manifest_path).expect("read manifest");
    assert_eq!(manifest.batch_id, "test-batch-39");
    assert_eq!(manifest.overflow_count, 2);
    assert_eq!(manifest.tasks.len(), 2);
    assert_eq!(manifest.tasks[0].id, "task-3");
    assert_eq!(manifest.tasks[1].id, "task-4");

    let expected_trailer = format!("2 more task results: local://{}", manifest_path.display());
    assert!(res.message.contains(&expected_trailer), "message must contain trailer line");

    // 2. All children fit within budget: no overflow manifest, no trailer line.
    let small_tasks: Vec<ChildCompletion> = (0..2)
        .map(|i| ChildCompletion::new(format!("small-{i}"), "completed", "small output"))
        .collect();
    let res = build_wake_batch(WakeBatchOptions::new(state_dir), &small_tasks).expect("build small");
    assert_eq!(res.inline_tasks.len(), 2);
    assert!(res.overflow_tasks.is_empty());
    assert!(res.overflow_manifest_path.is_none());
    assert!(!res.message.contains("more task results:"));

    // 3. QA failure scenario: Configured batch limit below minimum (2,000) clamps to minimum.
    let medium_tasks: Vec<ChildCompletion> = (0..3)
        .map(|i| ChildCompletion::new(format!("m-{i}"), "completed", "C".repeat(1_500)))
        .collect();
    let options = WakeBatchOptions::new(state_dir).with_limit(100);
    let res = build_wake_batch(options, &medium_tasks).expect("build clamped batch");
    assert_eq!(res.inline_tasks.len(), 1);
    assert_eq!(res.overflow_tasks.len(), 2);
    assert!(count_utf16_code_units(&res.message) <= MIN_COMPLETION_BATCH_LIMIT);
}

/// Review Finding 5: Wired path driving settled task delivery through extension code.
#[test]
fn wired_delivery_caps_body_with_pointer_and_intact_envelope() {
    let dir = tempdir().expect("tempdir");
    let state_dir = dir.path();
    let sink = Arc::new(MessageSink::default());
    let api = test_api(Arc::clone(&sink));
    let record = sample_record("wired-task-1");

    let raw_10k = "Z".repeat(10_000);
    deliver_settled(SettledDelivery {
        sender: &api,
        state_dir,
        record: &record,
        raw_response: &raw_10k,
        limit: 8_000,
    })
    .expect("deliver");

    let messages = sink.messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "exactly one message delivered");
    let msg = &messages[0];
    assert_eq!(msg.custom_type, "task-result");

    let content = get_content_text(msg);

    // Envelope must remain intact: opening tag, output, resume, closing tag
    assert!(content.starts_with("<task-result id=\"wired-task-1\""), "starts with opening tag");
    assert!(content.contains("<output>"), "has output opening tag");
    assert!(content.contains("</output>"), "has output closing tag");
    assert!(content.contains("<resume>"), "has resume tag");
    assert!(content.ends_with("</task-result>"), "ends with closing tag");

    // Visible pointer line has BOTH local:// URI AND plain absolute path
    let expected_spill = state_dir.join("completion-results/wired-task-1.txt");
    let expected_uri = format!("local://{}", expected_spill.display());
    let expected_abs = expected_spill.display().to_string();
    assert!(content.contains(&expected_uri), "must contain local:// URI");
    assert!(content.contains(&expected_abs), "must contain plain absolute path");
    let expected_pointer = format!("Full result: {expected_uri} ({expected_abs})");
    assert!(content.contains(&expected_pointer), "must contain pointer line");

    // Spill file holds child raw response, not the XML envelope
    assert!(expected_spill.is_file(), "spill file must exist");
    let spilled_content = std::fs::read_to_string(&expected_spill).expect("read spill");
    assert_eq!(spilled_content, raw_10k, "spill file holds raw response");
    assert!(!spilled_content.contains("<task-result"), "spill file must not contain envelope");

    let details = msg.details.as_ref().expect("details");
    assert_eq!(details["id"], "wired-task-1");
    assert_eq!(details["status"], "completed");
    assert_eq!(details["spilled"], true);
    assert_eq!(details["file_uri"], expected_uri);
    assert_eq!(details["file_path"], expected_abs);
}

/// Review Finding 3: Failed spill write still slices with visible note.
#[test]
fn wired_delivery_failed_spill_still_slices_with_note() {
    let dir = tempdir().expect("tempdir");
    let sink = Arc::new(MessageSink::default());
    let api = test_api(Arc::clone(&sink));
    let blocked_dir = dir.path().join("completion-results");
    std::fs::write(&blocked_dir, "blocker").expect("write blocker");
    let record = sample_record("blocked-task");

    let raw_10k = "K".repeat(10_000);
    deliver_settled(SettledDelivery {
        sender: &api,
        state_dir: dir.path(),
        record: &record,
        raw_response: &raw_10k,
        limit: 8_000,
    })
    .expect("deliver");

    let messages = sink.messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    let content = get_content_text(&messages[0]);

    assert!(content.contains("full result could not be saved:"), "must contain failure note");
    assert!(content.ends_with("</task-result>"), "envelope must remain intact");
    assert!(content.contains("<output>"));
    assert!(content.contains("</output>"));
}

/// Review Finding 5: Wired batch delivery caps and overflows.
#[test]
fn wired_batch_delivery_caps_and_overflows() {
    let dir = tempdir().expect("tempdir");
    let state_dir = dir.path();
    let sink = Arc::new(MessageSink::default());
    let api = test_api(Arc::clone(&sink));

    let tasks: Vec<ChildCompletion> = (0..5)
        .map(|i| {
            let record = sample_record(&format!("batch-task-{i}"));
            let raw_body = format!("Task {i} output: {}", "X".repeat(5_970));
            let (envelope, details) = build_capped_result_envelope(state_dir, &record, &raw_body, 8_000);
            ChildCompletion::new(format!("batch-task-{i}"), "completed", envelope).with_details(details)
        })
        .collect();

    let res = deliver_batch(&api, state_dir, 24_000, &tasks).expect("deliver batch");

    let messages = sink.messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "batched delivery sends one wake message");
    let content = get_content_text(&messages[0]);

    assert_eq!(res.inline_tasks.len(), 3);
    assert_eq!(res.overflow_tasks.len(), 2);
    assert!(count_utf16_code_units(content) <= 24_000);

    for i in 0..3 {
        assert!(content.contains(&format!("id=\"batch-task-{i}\"")));
    }
    for i in 3..5 {
        assert!(!content.contains(&format!("id=\"batch-task-{i}\"")));
    }

    let manifest_path = res.overflow_manifest_path.expect("manifest path");
    let expected_trailer = format!("2 more task results: local://{}", manifest_path.display());
    assert!(content.contains(&expected_trailer));

    let manifest = read_overflow_manifest(&manifest_path).expect("read manifest");
    assert_eq!(manifest.overflow_count, 2);
    assert_eq!(manifest.tasks[0].id, "batch-task-3");
    assert_eq!(manifest.tasks[1].id, "batch-task-4");
}

/// Defect B / Fix 5: Failed overflow manifest write still delivers inline tasks with visible note.
#[test]
fn wired_batch_delivery_failed_manifest_delivers_inline_with_note() {
    let dir = tempdir().expect("tempdir");
    let state_dir = dir.path();
    let sink = Arc::new(MessageSink::default());
    let api = test_api(Arc::clone(&sink));

    // Block completion-results directory so manifest write fails
    let blocked = state_dir.join("completion-results");
    std::fs::write(&blocked, "blocker").expect("write blocker");

    let tasks: Vec<ChildCompletion> = (0..5)
        .map(|i| {
            let record = sample_record(&format!("block-task-{i}"));
            let raw_body = format!("Task {i} output: {}", "B".repeat(5_970));
            let (envelope, details) = build_capped_result_envelope(state_dir, &record, &raw_body, 8_000);
            ChildCompletion::new(format!("block-task-{i}"), "completed", envelope).with_details(details)
        })
        .collect();

    let res = deliver_batch(&api, state_dir, 24_000, &tasks).expect("deliver batch succeeds");

    assert_eq!(res.inline_tasks.len(), 3);
    assert_eq!(res.overflow_tasks.len(), 2);
    assert!(res.overflow_manifest_path.is_none());
    assert!(res.manifest_error.is_some());

    let messages = sink.messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "one wake message delivered despite manifest write error");
    let content = get_content_text(&messages[0]);

    assert!(content.contains("2 more task results could not be saved:"), "trailer note present");
    assert!(count_utf16_code_units(content) <= 24_000, "error trailer keeps the batch limit");
    for i in 0..3 {
        assert!(content.contains(&format!("id=\"block-task-{i}\"")), "inline task {i} present");
    }
}

/// Defect C / Fix 6 & 7: Production settle through TaskManager delivers before waiters wake.
#[tokio::test]
async fn settled_batcher_delivers_before_waiters_wake() {
    let dir = tempdir().expect("tempdir");
    let runner = FakeRunner::new();
    runner.outputs.lock().unwrap().insert("bg-settle-1".to_owned(), "task completed successfully\n".to_owned());

    let sink = Arc::new(MessageSink::default());
    let api = test_api(Arc::clone(&sink));
    let manager = test_manager(dir.path(), Arc::clone(&runner));
    bind_settled_batcher(&manager, Arc::new(api), 8_000, 24_000);

    let record = manager.start(spawn_request(dir.path(), "bg-settle-1", "work on thing")).expect("start");
    let settled = manager.wait(&record.id, Some(Duration::from_secs(5))).await.expect("settle");
    assert_eq!(settled.status, TaskStatus::Completed);

    // Invariant: Final background delivery happens BEFORE waiters wake.
    let messages = sink.messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "delivery message sent before wait returned");
    let content = get_content_text(&messages[0]);
    assert!(content.starts_with("<task-result id=\"bg-settle-1\""));
    assert!(content.contains("<output>\ntask completed successfully\n</output>"));
    assert!(content.ends_with("</task-result>"));
}

/// Defect A / Fix 4: Failed task with no .md output uses stderr tail without nested envelopes.
#[tokio::test]
async fn settled_batcher_failed_task_stderr_not_nested() {
    let dir = tempdir().expect("tempdir");
    let runner = FakeRunner::new();
    runner.failures.lock().unwrap().insert("bg-fail-1".to_owned());
    runner.logs.lock().unwrap().insert("bg-fail-1".to_owned(), "fatal error: stack overflow\n".to_owned());

    let sink = Arc::new(MessageSink::default());
    let api = test_api(Arc::clone(&sink));
    let manager = test_manager(dir.path(), Arc::clone(&runner));
    bind_settled_batcher(&manager, Arc::new(api), 8_000, 24_000);

    let record = manager.start(spawn_request(dir.path(), "bg-fail-1", "crash task")).expect("start");
    let settled = manager.wait(&record.id, Some(Duration::from_secs(5))).await.expect("settle");
    assert_eq!(settled.status, TaskStatus::Failed);

    let messages = sink.messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    let content = get_content_text(&messages[0]);

    assert!(content.contains("fatal error: stack overflow"), "stderr tail present");
    assert_eq!(content.matches("<task-result").count(), 1, "exactly one opening envelope tag (never nested)");
    assert_eq!(content.matches("</task-result>").count(), 1, "exactly one closing envelope tag");
    assert!(content.contains("status=\"failed (exit 1)\""));
}

/// Sink whose first `send_message` blocks until the test releases it, so later settles queue up
/// behind a delivery in flight.
struct GatedSink {
    messages: Mutex<Vec<CustomMessage>>,
    gate: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl ExtensionActions for GatedSink {
    fn send_message(&self, msg: CustomMessage, _: SendMessageOptions) -> Result<(), ExtensionFailure> {
        let gate = self.gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        self.messages.lock().unwrap().push(msg);
        Ok(())
    }
    fn send_user_message(&self, _: UserMessageContent, _: SendUserMessageOptions) -> Result<(), ExtensionFailure> { Ok(()) }
    fn append_entry(&self, _: &str, _: Option<serde_json::Value>) -> Result<(), ExtensionFailure> { Ok(()) }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> { Ok(Vec::new()) }
}

/// Spec §9.2: results that settle while a delivery is in flight coalesce into one bounded wake.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn settled_batcher_coalesces_concurrent_tasks_through_manager() {
    let dir = tempdir().expect("tempdir");
    let runner = FakeRunner::new();
    for i in 0..4 {
        runner.outputs.lock().unwrap().insert(format!("coalesce-{i}"), format!("concurrent out {i}"));
    }
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let sink = Arc::new(GatedSink {
        messages: Mutex::default(),
        gate: Mutex::new(Some((entered_tx, release_rx))),
    });
    let runtime = ExtensionRuntime::default();
    runtime.bind(Arc::clone(&sink) as Arc<dyn ExtensionActions>);
    let api = ExtensionApi::new(LoadedExtension::new("task", "/tmp".into(), Default::default()), ExtensionSessionProfile::default(), Default::default(), runtime);
    let manager = test_manager(dir.path(), Arc::clone(&runner));
    let batcher = bind_settled_batcher(&manager, Arc::new(api), 8_000, 24_000);

    let ids: Vec<String> = (0..4)
        .map(|i| {
            let name = format!("coalesce-{i}");
            manager.start(spawn_request(dir.path(), &name, &format!("hold {i}"))).expect("start").id
        })
        .collect();

    // Given: the first settled result is blocked inside its delivery.
    runner.release.add_permits(1);
    entered_rx.recv_timeout(Duration::from_secs(5)).expect("first delivery in flight");
    // When: the other three settle while that delivery is in flight.
    runner.release.add_permits(3);
    assert!(batcher.wait_queued(3, Duration::from_secs(5)), "three results queued behind the flush");
    release_tx.send(()).unwrap();
    for id in &ids {
        manager.wait(id, Some(Duration::from_secs(5))).await.expect("settle");
    }

    // Then: one wake for the first result and one coalesced wake for the other three.
    let messages = sink.messages.lock().unwrap();
    assert_eq!(messages.len(), 2, "queued results coalesce into one message");
    let first = get_content_text(&messages[0]);
    let second = get_content_text(&messages[1]);
    assert!(count_utf16_code_units(second) <= 24_000);
    let in_first = ids.iter().filter(|id| first.contains(&format!("id=\"{id}\""))).count();
    let in_second = ids.iter().filter(|id| second.contains(&format!("id=\"{id}\""))).count();
    assert_eq!((in_first, in_second), (1, 3));
}

/// Spec §9.2: a single overflowed task does not carry its full response in the wake details.
#[test]
fn single_overflowed_task_details_stay_bounded() {
    let dir = tempdir().expect("tempdir");
    let sink = Arc::new(MessageSink::default());
    let api = test_api(Arc::clone(&sink));
    let record = sample_record("big-task");
    let (envelope, details) = build_capped_result_envelope(dir.path(), &record, &"Q".repeat(7_000), 8_000);
    let task = ChildCompletion::new("big-task", "completed", envelope).with_details(details);

    let res = deliver_batch(&api, dir.path(), MIN_COMPLETION_BATCH_LIMIT, &[task]).expect("deliver");

    assert_eq!(res.overflow_tasks.len(), 1);
    let messages = sink.messages.lock().unwrap();
    let details = messages[0].details.as_ref().expect("details");
    assert_eq!(details["overflow_count"], 1);
    assert!(details.get("response").is_none() && details.get("body").is_none());
    assert!(count_utf16_code_units(get_content_text(&messages[0])) <= MIN_COMPLETION_BATCH_LIMIT);
}
