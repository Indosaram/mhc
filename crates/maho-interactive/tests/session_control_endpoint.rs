//! Port of senpi test/interactive-session-control.test.ts, test/suite/session-control-ui-response.test.ts,
//! test/suite/regressions/4267-observing-reads-terminal-parity.test.ts (the terminal half) and
//! test/suite/regressions/3246-rpc-drain-notice-cap.test.ts (the terminal half), v2026.10.27-2.
//!
//! Each test binds a real endpoint on a real Unix socket over a real session and speaks to it the way
//! a client does: the 32-byte secret first, then one JSONL request per connection.

use maho_ai::types::{ContentBlock, Model, ModelThinkingLevel, StopReason, ToolCall};
use maho_core::extensions::session_control_types::{
    RegisterControlEndpointOptions, SessionControlRegistration,
};
use maho_ext_api::QuestionResponse;
use maho_interactive::session_control_commands::TuiControlSurface;
use maho_interactive::session_control_endpoint::{
    ControlEndpointOutcome, register_session_control_endpoint,
};
use maho_interactive::session_control_lifecycle::{ActiveControlEndpoint, TuiControlContext};
use maho_interactive::session_control_session_commands::TuiSessionControls;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, Weak};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct TestSurface {
    session_ref: Mutex<Option<Weak<maho_core::agent_session::AgentSession>>>,
    selected_models: Mutex<Vec<Model>>,
    selected_thinking: Mutex<Vec<(ModelThinkingLevel, bool)>>,
    interrupts: Mutex<usize>,
    draft: Mutex<Option<String>>,
    notices: Mutex<Vec<String>>,
    pending: Mutex<Vec<String>>,
    answered: Mutex<Vec<(String, QuestionResponse)>>,
}

impl TuiSessionControls for TestSurface {
    fn select_model(
        &self,
        model: Model,
    ) -> futures::future::BoxFuture<'static, Result<Option<String>, String>> {
        lock(&self.selected_models).push(model.clone());
        let session = lock(&self.session_ref).as_ref().and_then(|w| w.upgrade());
        Box::pin(async move {
            if let Some(session) = session {
                session
                    .set_session_model(model)
                    .await
                    .map(|change| change.and_then(|c| c.system_prompt_name))
            } else {
                Ok(None)
            }
        })
    }
    fn select_thinking_level(&self, level: ModelThinkingLevel, remember: bool) {
        lock(&self.selected_thinking).push((level, remember));
        let session = lock(&self.session_ref).as_ref().and_then(|w| w.upgrade());
        if let Some(session) = session {
            if remember {
                session.set_thinking_level(level);
                let current = session.thinking_level();
                if current != ModelThinkingLevel::Off || session.supports_thinking() {
                    let values: maho_core::settings_manager::Settings = [(
                        "defaultThinkingLevel".into(),
                        serde_json::json!(current.as_str()),
                    )]
                    .into_iter()
                    .collect();
                    let _ = session.with_settings_manager_mut(|settings| {
                        settings.set(maho_core::settings_manager::SettingsScope::Global, &values)
                    });
                }
            } else {
                session.set_session_thinking_level(level);
            }
        }
    }
    fn interrupt_turn(&self) -> futures::future::BoxFuture<'static, ()> {
        *lock(&self.interrupts) += 1;
        let session = lock(&self.session_ref).as_ref().and_then(|w| w.upgrade());
        Box::pin(async move {
            if let Some(session) = session {
                session.abort().await;
            }
        })
    }
}

impl TuiControlSurface for TestSurface {
    fn draft_hold(&self) -> Option<String> {
        lock(&self.draft).clone()
    }
    fn blocking_question(&self) -> bool {
        false
    }
    fn pending_question_ids(&self) -> Vec<String> {
        lock(&self.pending).clone()
    }
    fn pending_question(&self, request_id: &str) -> Option<Vec<Value>> {
        lock(&self.pending)
            .iter()
            .any(|id| id == request_id)
            .then(|| vec![json!({"id":"q1","header":"Ship","question":"Ship it?","options":[],"multiSelect":false})])
    }
    fn answer_question(&self, request_id: &str, response: QuestionResponse) -> bool {
        let mut pending = lock(&self.pending);
        let before = pending.len();
        pending.retain(|id| id != request_id);
        if pending.len() == before {
            return false;
        }
        lock(&self.answered).push((request_id.to_owned(), response));
        true
    }
    fn notice(&self, line: &str) {
        lock(&self.notices).push(line.to_owned());
    }
}

struct Fixture {
    endpoint: ActiveControlEndpoint,
    socket: String,
    agent_dir: std::path::PathBuf,
    inbox_dir: std::path::PathBuf,
    surface: Arc<TestSurface>,
    wakes: Arc<Mutex<Vec<Value>>>,
    session: Arc<maho_core::agent_session::AgentSession>,
    _mode: maho_interactive::interactive_mode::InteractiveMode,
    _directory: tempfile::TempDir,
}

async fn start_endpoint(
    drain: Option<maho_core::extensions::session_control_types::SessionControlDrain>,
) -> Fixture {
    start_endpoint_with(drain, false, None).await
}

/// `persisted`: a session file under the fixture directory (senpi `persistSession: true`).
async fn start_endpoint_with(
    drain: Option<maho_core::extensions::session_control_types::SessionControlDrain>,
    persisted: bool,
    is_session_referenced: Option<
        maho_core::extensions::session_control_types::IsSessionReferenced,
    >,
) -> Fixture {
    let (mode, directory, session) =
        crate::runtime35_native::native_mode_with_session_and_provider_retaining_session(
            None,
            |cwd| {
                if persisted {
                    let sessions = format!("{cwd}/sessions");
                    maho_core::session_manager::SessionManager::create(cwd, Some(&sessions), None)
                } else {
                    maho_core::session_manager::SessionManager::in_memory(cwd, None, None)
                }
            },
            "faux",
            "faux-1",
            false,
        );
    // A short agent directory keeps the socket under `<agentDir>/rpc/tui` within `sun_path`.
    let agent_dir = directory.path().join("a");
    let inbox_dir = directory.path().join("inbox");
    let surface = Arc::new(TestSurface::default());
    *lock(&surface.session_ref) = Some(Arc::downgrade(&session));
    let wakes = Arc::new(Mutex::new(Vec::new()));
    let seen = wakes.clone();
    let drain = drain.unwrap_or_else(|| {
        Arc::new(
            |_event: Value| -> futures::future::BoxFuture<'static, Result<Option<Value>, String>> {
                Box::pin(async { Ok(None) })
            },
        )
    });
    let options = RegisterControlEndpointOptions {
        inbox_dir: inbox_dir.to_string_lossy().into_owned(),
        drain: Arc::new(
            move |event: Value| -> futures::future::BoxFuture<'static, Result<Option<Value>, String>> {
                lock(&seen).push(event.clone());
                drain(event)
            },
        ),
        is_session_referenced,
    };
    let context = TuiControlContext {
        session: session.clone(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        surface: surface.clone(),
        editor_revision: Arc::new(|| 0),
    };
    let ControlEndpointOutcome::Registered {
        registration,
        endpoint,
    } = register_session_control_endpoint(context, options).await
    else {
        panic!("the endpoint registers: {:?}", lock(&surface.notices));
    };
    let SessionControlRegistration::Registered { socket, .. } = registration else {
        panic!("a registered outcome carries a registered registration");
    };
    Fixture {
        endpoint,
        socket,
        agent_dir,
        inbox_dir,
        surface,
        wakes,
        session,
        _mode: mode,
        _directory: directory,
    }
}

enum Reply {
    Answered(Value),
    Closed,
}

async fn control_request_with(socket: &str, command: Value, secret: &[u8]) -> Reply {
    let mut command = command;
    if command["id"].is_null() {
        command["id"] = json!(format!("req-{}", uuid::Uuid::new_v4()));
    }
    let id = command["id"].clone();
    let Ok(stream) = tokio::net::UnixStream::connect(socket).await else {
        return Reply::Closed;
    };
    let (read, mut write) = stream.into_split();
    if write.write_all(secret).await.is_err()
        || write
            .write_all(format!("{command}\n").as_bytes())
            .await
            .is_err()
    {
        return Reply::Closed;
    }
    let mut lines = tokio::io::BufReader::new(read).lines();
    let answered = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if record["id"] == id {
                return Reply::Answered(record);
            }
        }
        Reply::Closed
    })
    .await;
    answered.unwrap_or(Reply::Closed)
}

async fn control_request(socket: &str, command: Value) -> Value {
    let secret_path = maho_rpc::socket_transport::socket_secret_path(socket).expect("secret path");
    let secret = std::fs::read(secret_path).expect("secret");
    match control_request_with(socket, command, &secret).await {
        Reply::Answered(record) => record,
        Reply::Closed => panic!("the endpoint closed without answering"),
    }
}

async fn control_data(socket: &str, command: Value) -> Value {
    let record = control_request(socket, command).await;
    assert_eq!(record["success"], true, "{record}");
    record["data"].clone()
}

#[tokio::test]
async fn registers_as_a_tui_endpoint_and_removes_its_directory_on_dispose() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = start_endpoint(None).await;
    let name = std::path::Path::new(&fixture.socket)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    assert!(
        maho_rpc::host_endpoints::is_tui_control_socket(&name),
        "{name}"
    );
    let mode = |path: &str| std::fs::metadata(path).expect("stat").permissions().mode() & 0o777;
    assert_eq!(mode(&fixture.socket), 0o600);
    let secret = maho_rpc::socket_transport::socket_secret_path(&fixture.socket).expect("secret");
    assert_eq!(mode(&secret), 0o600);
    let parent = std::path::Path::new(&fixture.socket)
        .parent()
        .expect("parent")
        .to_string_lossy()
        .into_owned();
    assert_eq!(mode(&parent), 0o700);
    let paths =
        maho_rpc::host_daemon_paths::create_host_daemon_paths(&fixture.socket, &fixture.agent_dir);
    assert!(paths.dir.exists());
    assert_eq!(
        maho_rpc::host_endpoints::endpoint_kind_of_socket(&fixture.socket, &fixture.agent_dir),
        maho_rpc::host_endpoints::EndpointKind::Tui
    );

    fixture.endpoint.dispose().await;
    assert!(!paths.dir.exists());
    assert!(!std::path::Path::new(&fixture.socket).exists());
}

#[tokio::test]
async fn refuses_a_wrong_secret_before_answering_any_jsonl_command() {
    let fixture = start_endpoint(None).await;
    let reply = control_request_with(
        &fixture.socket,
        json!({"type": "get_protocol_info"}),
        &[7; 32],
    )
    .await;
    assert!(matches!(reply, Reply::Closed));
    let info = control_data(&fixture.socket, json!({"type": "get_protocol_info"})).await;
    assert_eq!(info["protocolVersion"], 1);
    assert_eq!(
        info["capabilities"],
        json!([
            "tui_control",
            "session_status",
            "tui_observer",
            "observe_reads",
            "entries_tail",
            "snapshot_cursor",
            "ui_answer_ledger",
        ])
    );
    assert_eq!(info["mode"], "tui");
    assert_eq!(info["endpoint_kind"], "tui");
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn answers_commands_outside_the_allowlist_as_unsupported_data() {
    let fixture = start_endpoint(None).await;
    for kind in [
        "bash",
        "cycle_model",
        "new_session",
        "switch_session",
        "nope",
    ] {
        let reply = control_request(&fixture.socket, json!({"type": kind, "message": "hi"})).await;
        assert_eq!(reply["success"], false);
        assert_eq!(reply["error"], "unsupported");
    }
    assert!(fixture.session.messages().is_empty());
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn serves_one_session_row_and_the_extended_state() {
    let fixture = start_endpoint(None).await;
    let listing = control_data(&fixture.socket, json!({"type": "list_sessions"})).await;
    assert_eq!(
        listing["sessions"][0]["sessionId"],
        fixture.session.session_id()
    );
    assert_eq!(listing["sessions"][0]["kind"], "interactive");
    assert_eq!(listing["sessions"][0]["surface"], "tui");
    let state = control_data(&fixture.socket, json!({"type": "get_state"})).await;
    assert_eq!(state["turn_epoch"], 0);
    assert_eq!(state["blocking_question"], false);
    assert_eq!(state["compacting"], false);
    assert_eq!(state["editor_has_draft"], false);
    *lock(&fixture.surface.draft) = Some("draft".into());
    let state = control_data(&fixture.socket, json!({"type": "get_state"})).await;
    assert_eq!(state["editor_has_draft"], true);
    let renamed = control_data(
        &fixture.socket,
        json!({"type": "set_session_name", "name": "tui-one"}),
    )
    .await;
    assert!(renamed.is_null());
    assert_eq!(fixture.session.session_name().as_deref(), Some("tui-one"));
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn wake_runs_the_drain_and_answers_its_admissions() {
    let fixture = start_endpoint(Some(Arc::new(
        |event: Value| -> futures::future::BoxFuture<'static, Result<Option<Value>, String>> {
            let admitted = event["delivery_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&json!("d2")));
            Box::pin(async move {
                Ok(admitted
                    .then(|| json!({"admitted": [{"delivery_id": "d2", "kind": "started"}]})))
            })
        },
    )))
    .await;
    assert_eq!(
        control_data(&fixture.socket, json!({"type": "wake"})).await,
        json!({"admitted": []})
    );
    assert_eq!(
        control_data(
            &fixture.socket,
            json!({"type": "wake", "delivery_ids": ["d2"]})
        )
        .await,
        json!({"admitted": [{"delivery_id": "d2", "kind": "started"}]})
    );
    let last = lock(&fixture.wakes).last().cloned().expect("a wake ran");
    assert_eq!(last["reason"], "command");
    assert_eq!(last["delivery_ids"], json!(["d2"]));
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn wakes_the_drain_with_reason_inbox_when_an_entry_is_created() {
    let (sender, mut woken) = tokio::sync::mpsc::unbounded_channel::<Value>();
    // Subscribed before the write: the drain reports every pass whose reasons include `inbox` after
    // the marker exists.
    let marker_slot: Arc<Mutex<Option<std::path::PathBuf>>> = Arc::default();
    let watched = marker_slot.clone();
    let fixture = start_endpoint(Some(Arc::new(
        move |event: Value| -> futures::future::BoxFuture<'static, Result<Option<Value>, String>> {
            let written = lock(&watched)
                .as_ref()
                .is_some_and(|marker| marker.exists());
            if written
                && event["reasons"]
                    .as_array()
                    .is_some_and(|reasons| reasons.contains(&json!("inbox")))
            {
                drop(sender.send(event));
            }
            Box::pin(async { Ok(None) })
        },
    )))
    .await;
    let marker = fixture.inbox_dir.join("d-inbox");
    *lock(&marker_slot) = Some(marker.clone());
    std::fs::write(&marker, "marker").expect("marker");
    let event = tokio::time::timeout(std::time::Duration::from_secs(10), woken.recv())
        .await
        .expect("an inbox wake")
        .expect("the drain reported");
    assert!(
        event["reasons"]
            .as_array()
            .is_some_and(|reasons| reasons.contains(&json!("inbox")))
    );
    fixture.endpoint.dispose().await;
}

// senpi#3246 (3246-rpc-drain-notice-cap, the terminal half): a drain that keeps failing reports once,
// not on every wake, until a pass succeeds again. Each wake answers only after its pass and its
// failure report ran, so the counts are read without waiting.
#[tokio::test]
async fn a_drain_that_keeps_failing_reports_once_until_a_pass_succeeds() {
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let drain_failing = failing.clone();
    let fixture = start_endpoint(Some(Arc::new(
        move |_event: Value| -> futures::future::BoxFuture<'static, Result<Option<Value>, String>> {
            let failing = drain_failing.load(std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if failing {
                    return Err("inbox store unreadable".to_owned());
                }
                Ok(None)
            })
        },
    )))
    .await;
    let reports = |surface: &TestSurface| {
        lock(&surface.notices)
            .iter()
            .filter(|line| line.contains("drain failed"))
            .count()
    };
    for _ in 0..3 {
        control_data(&fixture.socket, json!({"type": "wake"})).await;
    }
    let while_failing = reports(&fixture.surface);
    failing.store(false, std::sync::atomic::Ordering::SeqCst);
    control_data(&fixture.socket, json!({"type": "wake"})).await;
    failing.store(true, std::sync::atomic::Ordering::SeqCst);
    control_data(&fixture.socket, json!({"type": "wake"})).await;
    assert_eq!(
        (while_failing, reports(&fixture.surface)),
        (1, 2),
        "{:?}",
        lock(&fixture.surface.notices)
    );
    assert!(
        lock(&fixture.surface.notices)
            .iter()
            .all(|line| !line.contains("drain failed") || line.contains("inbox store unreadable"))
    );
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn streams_state_events_to_a_subscriber_from_its_cursor() {
    let fixture = start_endpoint(None).await;
    assert_eq!(
        control_data(&fixture.socket, json!({"type": "subscribe"})).await,
        json!({"cursor": 0})
    );
    fixture.endpoint.note_state();
    let replay = control_data(&fixture.socket, json!({"type": "subscribe", "cursor": 0})).await;
    assert_eq!(replay, json!({"cursor": 1}));
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn extension_ui_response_answers_with_the_frame_id_and_the_ledger_disposition() {
    let fixture = start_endpoint(None).await;
    *lock(&fixture.surface.pending) = vec!["r1".into(), "r2".into()];
    let answer = json!({"selected": ["Yes"]});
    let applied = control_request(
        &fixture.socket,
        json!({"id": "frame-1", "type": "extension_ui_response", "uiRequestId": "r1", "answerId": "a1", "answers": {"q1": answer}}),
    )
    .await;
    assert_eq!(applied["id"], "frame-1");
    assert_eq!(applied["success"], true);
    assert_eq!(applied["disposition"], "applied");
    let replay = control_request(
        &fixture.socket,
        json!({"id": "frame-2", "type": "extension_ui_response", "uiRequestId": "r1", "answerId": "a1", "answers": {"q1": answer}}),
    )
    .await;
    assert_eq!(replay["success"], true);
    assert_eq!(replay["disposition"], "replay");
    let already = control_request(
        &fixture.socket,
        json!({"id": "frame-3", "type": "extension_ui_response", "uiRequestId": "r1", "answerId": "other", "answers": {"q1": answer}}),
    )
    .await;
    assert_eq!(already["error"], "already_answered");
    assert_eq!(already["disposition"], "already_answered");
    let invalid = control_request(
        &fixture.socket,
        json!({"id": "frame-4", "type": "extension_ui_response", "uiRequestId": "r2", "answerId": "", "answers": {"q1": answer}}),
    )
    .await;
    assert_eq!(invalid["error"], "invalid_answer_id");
    let unknown = control_request(
        &fixture.socket,
        json!({"id": "frame-5", "type": "extension_ui_response", "uiRequestId": "gone", "answerId": "a5", "answers": {"q1": answer}}),
    )
    .await;
    assert_eq!(unknown["error"], "unknown_extension_ui_request");
    assert_eq!(unknown["disposition"], "unknown");
    // The short form: no `uiRequestId`, the frame `id` names the question.
    let short = control_request(
        &fixture.socket,
        json!({"id": "r2", "type": "extension_ui_response", "answers": {"q1": answer}}),
    )
    .await;
    assert_eq!(short["id"], "r2");
    assert_eq!(short["disposition"], "applied");
    let answered = lock(&fixture.surface.answered).clone();
    assert_eq!(
        answered
            .iter()
            .map(|(id, response)| (id.clone(), response.resolved_by))
            .collect::<Vec<_>>(),
        vec![
            (
                "r1".to_owned(),
                Some(maho_ext_api::QuestionResolvedBy::ControlEndpoint)
            ),
            (
                "r2".to_owned(),
                Some(maho_ext_api::QuestionResolvedBy::ControlEndpoint)
            ),
        ]
    );
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn terminal_get_snapshot_and_attach_session_follow_the_host_contract() {
    let fixture = start_endpoint(None).await;
    let snapshot = control_request(
        &fixture.socket,
        json!({"type": "get_snapshot", "sessionId": "someone-else"}),
    )
    .await;
    assert_eq!(snapshot["error"], "unknown_session");
    let snapshot = control_data(&fixture.socket, json!({"type": "get_snapshot", "tail": 5})).await;
    assert!(snapshot["markerId"].is_string());
    assert!(snapshot["entries"].is_array());
    let handle = fixture.session.session_id();
    let mismatch = control_request(
        &fixture.socket,
        json!({"type": "attach_session", "sessionId": handle, "durableSessionId": "other"}),
    )
    .await;
    assert_eq!(mismatch["errorCode"], "session_identity_mismatch");
    let attached = control_data(
        &fixture.socket,
        json!({"type": "attach_session", "sessionId": handle, "durableSessionId": handle}),
    )
    .await;
    assert_eq!(attached, json!({"sessionId": handle, "attached": true}));
    let released = control_request(
        &fixture.socket,
        json!({"type": "release_attachment", "sessionId": handle}),
    )
    .await;
    // Each request is its own connection here, and that one never attached.
    assert_eq!(released["errorCode"], "session_not_attached");
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn a_stop_needs_the_interactive_user_role() {
    let fixture = start_endpoint(None).await;
    let stop = control_request(&fixture.socket, json!({"type": "abort", "intent": "stop"})).await;
    assert_eq!(stop["success"], false);
    assert_eq!(
        stop["error"],
        "stop_requires_interactive_client: declare interactive_user with set_client_info first"
    );
    let invalid = control_request(
        &fixture.socket,
        json!({"type": "set_client_info", "capabilities": [1]}),
    )
    .await;
    assert_eq!(invalid["error"], "capabilities must be an array of strings");
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn attached_terminal_stream_delivers_activity_revisions_in_order() {
    let fixture = start_endpoint(None).await;
    let secret_path =
        maho_rpc::socket_transport::socket_secret_path(&fixture.socket).expect("secret path");
    let secret = std::fs::read(secret_path).expect("secret");
    let socket = tokio::net::UnixStream::connect(&fixture.socket)
        .await
        .expect("connect");
    let (read, mut write) = socket.into_split();
    let mut lines = tokio::io::BufReader::new(read).lines();
    let handle = fixture.session.session_id();
    write.write_all(&secret).await.expect("authenticate");
    let request = json!({"id":"activity-attach", "type":"attach_session",
        "sessionId":handle,"durableSessionId":handle});
    write
        .write_all(format!("{request}\n").as_bytes())
        .await
        .expect("attach request");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("read attach")
                .expect("open socket");
            let record: Value = serde_json::from_str(&line).expect("attach record");
            if record["id"] == "activity-attach" {
                assert_eq!(record["success"], true, "{record}");
                break;
            }
        }
    })
    .await
    .expect("attach deadline");
    for (revision, count) in [(1, 1), (2, 2), (3, 1)] {
        fixture.session.emit_extension_event(
            "session_lifecycle",
            &json!({
                "type":"session_lifecycle", "sessionId":handle,
                "lifecycle":{"state":"running", "seq":100,
                    "activity":{"kind":"tool", "toolName":"bash", "toolCallId":"a",
                        "since":"2026-10-27T00:00:00.000Z", "outstanding":count},
                    "activityRevision":{"instanceId":"activity-owner", "revision":revision}}
            }),
        );
    }
    let records = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let mut records = Vec::new();
        while records.len() < 3 {
            let line = lines
                .next_line()
                .await
                .expect("read activity")
                .expect("open socket");
            let record: Value = serde_json::from_str(&line).expect("activity record");
            if record["type"] == "session_lifecycle" {
                records.push(record);
            }
        }
        records
    })
    .await
    .expect("activity delivery deadline");
    for (record, (revision, count)) in records.iter().zip([(1, 1), (2, 2), (3, 1)]) {
        assert_eq!(record["sessionId"], handle);
        let lifecycle = &record["lifecycle"];
        assert_eq!(lifecycle["seq"], 100);
        assert_eq!(lifecycle["activity"]["outstanding"], count);
        assert_eq!(lifecycle["activity"]["since"], "2026-10-27T00:00:00.000Z");
        assert_eq!(
            lifecycle["activityRevision"],
            json!({"instanceId":"activity-owner", "revision":revision})
        );
        assert!(lifecycle["observedAt"].is_string());
    }
    drop(write);
    drop(lines);
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn a_client_message_id_retry_replays_its_receipt_and_new_content_conflicts() {
    let fixture = start_endpoint(None).await;
    let first = control_data(
        &fixture.socket,
        json!({"type": "follow_up", "message": "hello", "clientMessageId": "m1"}),
    )
    .await;
    assert_eq!(first["clientMessageId"], "m1");
    let replay = control_data(
        &fixture.socket,
        json!({"type": "follow_up", "message": "hello", "clientMessageId": "m1"}),
    )
    .await;
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["duplicate"], true);
    assert_eq!(replay["disposition"], first["disposition"]);
    let conflict = control_request(
        &fixture.socket,
        json!({"type": "follow_up", "message": "different", "clientMessageId": "m1"}),
    )
    .await;
    assert_eq!(conflict["errorCode"], "client_message_id_conflict");
    let unserved = control_request(
        &fixture.socket,
        json!({"type": "steer", "message": "x", "images": []}),
    )
    .await;
    assert_eq!(unserved["errorCode"], "unsupported_field");
    fixture.endpoint.dispose().await;
}

/// senpi `bindCurrentSessionExtensions` -> `session.setControlEndpointHost(sessionControlHost)`: once
/// the terminal binds extensions, `pi.session.registerControlEndpoint` (the generation's
/// `create_session_control_actions`) serves a TUI endpoint through the terminal's host; before the
/// binding no host is offered and the registration answers `unsupported_mode`.
#[tokio::test]
async fn pi_session_registers_through_the_terminal_host_once_extensions_are_bound() {
    use maho_core::extensions::session_control_types::{
        ControlEndpointUnsupported, SessionControlActions,
    };
    let (mut mode, directory, session) =
        crate::runtime35_native::native_mode_with_session_and_provider_retaining_session(
            None,
            |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
            "faux",
            "faux-1",
            false,
        );
    let options = || RegisterControlEndpointOptions {
        inbox_dir: directory
            .path()
            .join("inbox")
            .to_string_lossy()
            .into_owned(),
        drain: Arc::new(
            |_event: Value| -> futures::future::BoxFuture<'static, Result<Option<Value>, String>> {
                Box::pin(async { Ok(None) })
            },
        ),
        is_session_referenced: None,
    };
    // The first extension generation's `pi.session`, before the terminal offers its host.
    session
        .set_extension_runner(maho_ext_host::ExtensionRunner::from_static(
            Vec::new(),
            session.extension_context(mode.extension_ui.clone()),
        ))
        .await;
    let unbound = session.session_control().expect("first generation");
    assert!(matches!(
        unbound.register_control_endpoint(options()).await,
        SessionControlRegistration::Unsupported {
            reason: ControlEndpointUnsupported::UnsupportedMode
        }
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), mode.bind_extensions())
        .await
        .expect("bounded binding");
    let actions = session
        .session_control()
        .expect("the bound generation's pi.session");
    let SessionControlRegistration::Registered { socket, dispose } =
        actions.register_control_endpoint(options()).await
    else {
        panic!("the terminal host serves the registration");
    };
    let name = std::path::Path::new(&socket)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    assert!(
        maho_rpc::host_endpoints::is_tui_control_socket(&name),
        "{name}"
    );
    let listing = control_data(&socket, json!({"type": "list_sessions"})).await;
    assert_eq!(listing["sessions"][0]["sessionId"], session.session_id());
    assert_eq!(listing["sessions"][0]["surface"], "tui");
    dispose().await.expect("dispose");
    assert!(!std::path::Path::new(&socket).exists());
    mode.dispose_session_control().await;
}

type SeenWakes = Arc<Mutex<Vec<(String, Vec<String>, Option<Vec<String>>)>>>;

/// An extension that records each `session_control_wake` it sees and fails while `failing` is set.
struct WakeWatcher {
    seen: SeenWakes,
    failing: Arc<std::sync::atomic::AtomicBool>,
}

impl maho_ext_api::Extension for WakeWatcher {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let seen = self.seen.clone();
        let failing = self.failing.clone();
        api.on(
            maho_ext_api::EventKind::SessionControlWake,
            Arc::new(move |event, _ctx| {
                let seen = seen.clone();
                let failing = failing.load(std::sync::atomic::Ordering::SeqCst);
                Box::pin(async move {
                    if let maho_ext_api::ExtensionEvent::SessionControlWake {
                        reason,
                        reasons,
                        delivery_ids,
                    } = event
                    {
                        lock(&seen).push((reason.clone(), reasons.clone(), delivery_ids.clone()));
                    }
                    if failing {
                        return Err(maho_ext_api::ExtensionFailure::new("wake handler broke"));
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
    }
}

// senpi `session-control-endpoint.ts`: each wake pass emits `session_control_wake` into the session's
// extensions before the registrant's drain; a failing handler is an extension error, not a failed pass.
#[tokio::test]
async fn a_wake_is_emitted_into_the_sessions_extensions_before_the_drain() {
    let fixture = start_endpoint(None).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    fixture
        .session
        .set_extension_runner(maho_ext_host::ExtensionRunner::from_static(
            vec![Box::new(WakeWatcher {
                seen: seen.clone(),
                failing: failing.clone(),
            })],
            fixture
                .session
                .extension_context(fixture._mode.extension_ui.clone()),
        ))
        .await;
    // The endpoint's own startup passes (`inbox`, immediate and once the watch is armed) run
    // asynchronously and may land after the watcher is installed; only requested wakes count here.
    let requested = || {
        lock(&seen)
            .iter()
            .filter(|(_, reasons, _)| reasons.iter().any(|reason| reason != "inbox"))
            .cloned()
            .collect::<Vec<_>>()
    };
    let drained = || {
        lock(&fixture.wakes)
            .iter()
            .filter(|event| {
                event["reasons"]
                    .as_array()
                    .is_some_and(|reasons| reasons.iter().any(|reason| reason != "inbox"))
            })
            .count()
    };
    control_data(
        &fixture.socket,
        json!({"type": "wake", "delivery_ids": ["d1"]}),
    )
    .await;
    // A pending startup `inbox` edge may share the command's batch; `command` outranks it.
    let first = requested();
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].0, "command");
    assert!(first[0].1.contains(&"command".to_owned()), "{first:?}");
    assert_eq!(first[0].2, Some(vec!["d1".to_owned()]));
    assert_eq!(drained(), 1, "the drain still ran");
    // senpi's `emit` reports a handler's failure through `emitError`; the drain still runs.
    failing.store(true, std::sync::atomic::Ordering::SeqCst);
    control_data(&fixture.socket, json!({"type": "wake"})).await;
    assert_eq!(requested().len(), 2);
    assert_eq!(drained(), 2, "a failing handler does not skip the drain");
    assert!(
        lock(&fixture.surface.notices)
            .iter()
            .all(|line| !line.contains("drain failed")),
        "{:?}",
        lock(&fixture.surface.notices)
    );
    fixture.endpoint.dispose().await;
}

// senpi interactive-session-control-lifecycle.test.ts: the header-only rule at clean exit.
#[tokio::test]
async fn keeps_a_referenced_header_only_session_file_and_removes_an_unreferenced_one() {
    for referenced in [true, false] {
        let fixture = start_endpoint_with(
            None,
            true,
            Some(Arc::new(
                move || -> futures::future::BoxFuture<'static, bool> {
                    Box::pin(async move { referenced })
                },
            )),
        )
        .await;
        let file = fixture.session.session_file().expect("session file");
        let header = std::fs::read_to_string(&file).expect("header written");
        assert_eq!(header.trim().lines().count(), 1, "{header}");
        fixture.endpoint.dispose().await;
        assert_eq!(std::path::Path::new(&file).exists(), referenced);
    }
}

#[tokio::test]
async fn keeps_the_file_after_a_delivery_was_applied_even_when_unreferenced() {
    let fixture = start_endpoint_with(
        None,
        true,
        Some(Arc::new(|| -> futures::future::BoxFuture<'static, bool> {
            Box::pin(async { false })
        })),
    )
    .await;
    fixture.session.with_session_manager_mut(|manager| {
        manager.append_custom_message(
            "session_control_delivery",
            json!("m1"),
            true,
            Some(json!({"delivery_id": "d1"})),
        )
    });
    let file = fixture.session.session_file().expect("session file");
    fixture.endpoint.dispose().await;
    assert!(
        std::fs::read_to_string(&file)
            .expect("kept")
            .contains(r#""delivery_id":"d1""#)
    );
}

// senpi 4267-observing-reads-terminal-parity (the terminal half): the terminal's `abort` runs the host's
// `runAbort` over the session's effect ledger, so an `effectId` gets a receipt that `get_effect_status`
// reads back, and `awaitIdle`/`clearQueue` answer like a host.
#[tokio::test]
async fn abort_effects_and_effect_status_are_served_from_the_sessions_ledger() {
    let fixture = start_endpoint(None).await;
    let aborted = control_data(
        &fixture.socket,
        json!({"type": "abort", "effectId": "e-abort-1"}),
    )
    .await;
    assert_eq!(aborted["effectId"], "e-abort-1");
    assert_eq!(aborted["state"], "done");
    assert_eq!(aborted["effect"]["disposition"], "no_active_turn");
    let status = control_data(
        &fixture.socket,
        json!({"type": "get_effect_status", "effectId": "e-abort-1"}),
    )
    .await;
    assert_eq!(status["found"], true, "{status}");
    assert_eq!(status["effect"]["effectId"], "e-abort-1");
    let replay = control_data(
        &fixture.socket,
        json!({"type": "abort", "effectId": "e-abort-1"}),
    )
    .await;
    assert_eq!(replay["replayed"], true, "{replay}");
    let quiesced = control_data(
        &fixture.socket,
        json!({"type": "abort", "awaitIdle": true, "clearQueue": true}),
    )
    .await;
    assert_eq!(quiesced["idle"], true, "{quiesced}");
    assert_eq!(quiesced["cleared"], 0);
    let missing = control_data(
        &fixture.socket,
        json!({"type": "get_effect_status", "effectId": "never-sent"}),
    )
    .await;
    assert_eq!(missing, json!({"found": false, "searched": "complete"}));
    fixture.endpoint.dispose().await;
}

struct SocketClient {
    write: tokio::net::unix::OwnedWriteHalf,
    lines: tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    records: Vec<Value>,
    next: u32,
}

impl SocketClient {
    async fn connect(socket_path: &str) -> Self {
        let secret_path =
            maho_rpc::socket_transport::socket_secret_path(socket_path).expect("secret path");
        let secret = std::fs::read(secret_path).expect("secret");
        let stream = tokio::net::UnixStream::connect(socket_path)
            .await
            .expect("connect");
        let (read, mut write) = stream.into_split();
        write.write_all(&secret).await.expect("authenticate");
        let lines = tokio::io::BufReader::new(read).lines();
        Self {
            write,
            lines,
            records: Vec::new(),
            next: 0,
        }
    }

    async fn request(&mut self, mut command: Value) -> Value {
        self.next += 1;
        let id = format!("req-{}", self.next);
        command["id"] = json!(id);
        self.write
            .write_all(format!("{command}\n").as_bytes())
            .await
            .expect("write command");
        let target_id = id.clone();
        self.wait_for(move |record| record["id"] == target_id).await
    }

    async fn wait_for<F>(&mut self, predicate: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        if let Some(pos) = self.records.iter().position(&predicate) {
            return self.records.remove(pos);
        }
        let deadline = std::time::Duration::from_secs(10);
        tokio::time::timeout(deadline, async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .expect("read line")
                    .expect("stream open");
                let record: Value = serde_json::from_str(&line).expect("parse json");
                if predicate(&record) {
                    return record;
                }
                self.records.push(record);
            }
        })
        .await
        .expect("timed out waiting for matching record")
    }
}

async fn start_endpoint_with_provider_models(
    models: Vec<maho_ai::providers::faux::FauxModelDefinition>,
) -> Fixture {
    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message,
        faux_provider, faux_streams,
    };
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};

    let directory = tempfile::tempdir().expect("directory");
    let cwd = directory.path().to_string_lossy().into_owned();
    let initial_model_id = models[0].id.clone();
    let provider = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".into()),
        provider: Some("faux".into()),
        models: Some(models),
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("valid faux provider");
    let model = provider.get_model(Some(&initial_model_id)).expect("model");
    provider.set_responses(vec![
        faux_assistant_message(
            "hello",
            FauxAssistantMessageOptions {
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context = context.clone();
        context.system_prompt = context.system_prompt.filter(|prompt| !prompt.is_empty());
        streams.stream_simple(model, &context, options.map(|options| options.simple))
    });
    // senpi harness: the faux provider is seeded with an api_key credential so its models are
    // available (get_available_models / set_model consult availability, not the raw catalog).
    let auth_path = directory.path().join("auth.json");
    maho_core::auth_storage::AuthStorage::create(&auth_path.to_string_lossy())
        .set("faux", Some(json!({"type": "api_key", "key": "faux-key"})))
        .expect("seed faux credential");
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(directory.path().join("models.json")),
            auth_path: Some(auth_path),
            providers: Some(vec![provider.provider.clone()]),
            agent_dir: Some(directory.path().to_owned()),
            allow_model_network: Some(false),
            refresh_on_create: Some(false),
            ..Default::default()
        },
    )
    .expect("valid model runtime");
    let session = AgentSession::new(AgentSessionConfig {
        agent: maho_agent::Agent::new(maho_agent::AgentOptions {
            initial_state: Some(maho_agent::agent::PartialAgentState {
                model: Some(model),
                ..Default::default()
            }),
            stream_fn: Some(stream_fn),
            ..Default::default()
        }),
        session_manager: maho_core::session_manager::SessionManager::in_memory(&cwd, None, None),
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(
            Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),
            false,
        ),
        cwd: cwd.clone(),
        agent_dir: Some(cwd),
        fallback_now: Some(Arc::new(|| 0.0)),
        retry_random: Some(Arc::new(|| 0.5)),
        scoped_models: Vec::new(),
        favorite_models: Vec::new(),
        flag_values: Default::default(),
        custom_tools: Vec::new(),
        model_runtime: Some(runtime),
        model_registry: None,
        uses_default_stream_function: Some(false),
        initial_active_tool_names: None,
        default_tool_names: None,
        eval_only_tool_names: None,
        allowed_tool_names: None,
        excluded_tool_names: None,
        base_tools_override: None,
        session_start_event: None,
        auto_title_sessions: Some(false),
    })
    .expect("session");
    let session = Arc::new(session);
    struct TuiEditorHost;
    impl maho_tui::components::editor::EditorTuiHost for TuiEditorHost {
        fn request_render(&self) {}
        fn terminal_rows(&self) -> usize {
            36
        }
    }
    let mode = maho_interactive::interactive_mode::InteractiveMode::new(
        session.clone(),
        maho_interactive::theme::Theme::builtin(
            "dark",
            maho_interactive::theme::ColorMode::Truecolor,
        )
        .expect("theme"),
        std::rc::Rc::new(TuiEditorHost),
    );

    let agent_dir = directory.path().join("a");
    let inbox_dir = directory.path().join("inbox");
    let surface = Arc::new(TestSurface::default());
    *lock(&surface.session_ref) = Some(Arc::downgrade(&session));
    let wakes = Arc::new(Mutex::new(Vec::new()));
    let options = RegisterControlEndpointOptions {
        inbox_dir: inbox_dir.to_string_lossy().into_owned(),
        drain: Arc::new(|_event: Value| Box::pin(async { Ok(None) })),
        is_session_referenced: None,
    };
    let context = TuiControlContext {
        session: session.clone(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        surface: surface.clone(),
        editor_revision: Arc::new(|| 0),
    };
    let ControlEndpointOutcome::Registered {
        registration,
        endpoint,
    } = register_session_control_endpoint(context, options).await
    else {
        panic!("registration failed");
    };
    let SessionControlRegistration::Registered { socket, .. } = registration else {
        panic!("expected Registered");
    };
    Fixture {
        endpoint,
        socket,
        agent_dir,
        inbox_dir,
        surface,
        wakes,
        session,
        _mode: mode,
        _directory: directory,
    }
}

#[tokio::test]
async fn delivers_overlapping_activity_counts_1_2_1_through_tui_attachment_writer() {
    let fixture = start_endpoint(None).await;
    let mut client = SocketClient::connect(&fixture.socket).await;
    let handle = fixture.session.session_id();
    let attached = client
        .request(json!({
            "type": "attach_session",
            "sessionId": handle,
            "durableSessionId": handle,
        }))
        .await;
    assert_eq!(attached["success"], true, "{attached}");

    let (gate_a_tx, gate_a_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (gate_b_tx, gate_b_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (started_a_tx, mut started_a_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (started_b_tx, mut started_b_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let gate_a = Arc::new(tokio::sync::Mutex::new(gate_a_rx));
    let gate_b = Arc::new(tokio::sync::Mutex::new(gate_b_rx));
    let started_a = Arc::new(started_a_tx);
    let started_b = Arc::new(started_b_tx);

    let tool_a = maho_agent::types::AgentTool {
        label: "a".into(),
        prepare_arguments: None,
        execute: Arc::new(move |_, _, _, _| {
            let started_a = started_a.clone();
            let gate_a = gate_a.clone();
            Box::pin(async move {
                let _ = started_a.send(());
                let _ = gate_a.lock().await.recv().await;
                maho_agent::types::AgentToolResult::text("done a")
            })
        }),
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "a".into(),
            description: "Controlled tool a".into(),
            parameters: json!({"type": "object"}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
    };

    let tool_b = maho_agent::types::AgentTool {
        label: "b".into(),
        prepare_arguments: None,
        execute: Arc::new(move |_, _, _, _| {
            let started_b = started_b.clone();
            let gate_b = gate_b.clone();
            Box::pin(async move {
                let _ = started_b.send(());
                let _ = gate_b.lock().await.recv().await;
                maho_agent::types::AgentToolResult::text("done b")
            })
        }),
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "b".into(),
            description: "Controlled tool b".into(),
            parameters: json!({"type": "object"}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
    };

    fixture.session.agent().set_tools(vec![tool_a, tool_b]);
    fixture
        .session
        .agent()
        .set_tool_execution(maho_agent::types::ToolExecutionMode::Parallel);

    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message,
        faux_provider, faux_streams,
    };

    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("faux provider");
    provider.set_responses(vec![
        faux_assistant_message(
            vec![
                ContentBlock::ToolCall(ToolCall {
                    id: "call-a".into(),
                    name: "a".into(),
                    arguments: Default::default(),
                    ..Default::default()
                }),
                ContentBlock::ToolCall(ToolCall {
                    id: "call-b".into(),
                    name: "b".into(),
                    arguments: Default::default(),
                    ..Default::default()
                }),
            ],
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(
            "Done.",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Stop),
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context = context.clone();
        context.system_prompt = context.system_prompt.filter(|p| !p.is_empty());
        streams.stream_simple(model, &context, options.map(|o| o.simple))
    });
    fixture.session.agent().set_stream_function(stream_fn);

    let session = fixture.session.clone();
    let prompt_task =
        tokio::spawn(async move { session.prompt("Run both.", Default::default()).await });

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        started_a_rx.recv().await.expect("tool a started");
        started_b_rx.recv().await.expect("tool b started");
    })
    .await
    .expect("both tools started");

    gate_b_tx.send(()).expect("release b");

    let mut activity_records = Vec::new();
    let records = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while activity_records.len() < 3 {
            let record = client
                .wait_for(|r| {
                    r["type"] == "session_lifecycle" && r["lifecycle"]["activity"]["kind"] == "tool"
                })
                .await;
            activity_records.push(record);
        }
        activity_records
    })
    .await
    .expect("collected 3 activity records");

    gate_a_tx.send(()).expect("release a");
    prompt_task
        .await
        .expect("prompt task ok")
        .expect("prompt ok");

    let outstandings: Vec<i64> = records
        .iter()
        .map(|r| {
            r["lifecycle"]["activity"]["outstanding"]
                .as_i64()
                .unwrap_or(0)
        })
        .collect();
    assert_eq!(outstandings, vec![1, 2, 1]);

    let seq0 = &records[0]["lifecycle"]["seq"];
    for r in &records {
        assert_eq!(&r["lifecycle"]["seq"], seq0);
    }

    let since0 = &records[0]["lifecycle"]["activity"]["since"];
    for r in &records {
        assert_eq!(&r["lifecycle"]["activity"]["since"], since0);
    }

    let first_rev = &records[0]["lifecycle"]["activityRevision"];
    let inst_id = first_rev["instanceId"].as_str().expect("instanceId");
    let base_rev = first_rev["revision"].as_u64().expect("revision");
    for (i, r) in records.iter().enumerate() {
        assert_eq!(
            r["lifecycle"]["activityRevision"]["instanceId"].as_str(),
            Some(inst_id)
        );
        assert_eq!(
            r["lifecycle"]["activityRevision"]["revision"].as_u64(),
            Some(base_rev + i as u64)
        );
    }

    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn session_control_advertises_controls_and_lists_models_and_levels() {
    use maho_ai::providers::faux::FauxModelDefinition;
    let fixture = start_endpoint_with_provider_models(vec![
        FauxModelDefinition {
            id: "faux-reasoner".into(),
            name: Some("Faux Reasoner".into()),
            reasoning: Some(true),
            ..Default::default()
        },
        FauxModelDefinition {
            id: "faux-plain".into(),
            name: Some("Faux Plain".into()),
            ..Default::default()
        },
    ])
    .await;
    let info = control_data(&fixture.socket, json!({"type": "get_protocol_info"})).await;
    assert_eq!(info["mode"], "tui");
    let commands = info["commands"].as_array().expect("commands array");
    for cmd in [
        "set_model",
        "set_thinking_level",
        "interrupt",
        "set_session_name",
    ] {
        assert!(commands.iter().any(|c| c == cmd), "missing command {cmd}");
    }
    let listing = control_data(&fixture.socket, json!({"type": "get_available_models"})).await;
    let models = listing["models"].as_array().expect("models array");
    assert!(models.iter().any(|m| m["id"] == "faux-reasoner"));
    assert!(models.iter().any(|m| m["id"] == "faux-plain"));
    let levels = control_data(
        &fixture.socket,
        json!({"type": "get_available_thinking_levels"}),
    )
    .await;
    let expected: Vec<String> = fixture
        .session
        .get_available_thinking_levels()
        .into_iter()
        .map(|l| l.as_str().to_owned())
        .collect();
    let actual: Vec<String> = levels["levels"]
        .as_array()
        .expect("levels")
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    assert_eq!(actual, expected);
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn session_control_set_model_switches_model_and_records_control_source() {
    use maho_ai::providers::faux::FauxModelDefinition;
    let fixture = start_endpoint_with_provider_models(vec![
        FauxModelDefinition {
            id: "faux-plain".into(),
            name: Some("Faux Plain".into()),
            ..Default::default()
        },
        FauxModelDefinition {
            id: "faux-reasoner".into(),
            name: Some("Faux Reasoner".into()),
            reasoning: Some(true),
            ..Default::default()
        },
    ])
    .await;
    assert_eq!(fixture.session.model().id, "faux-plain");
    let default_before = fixture
        .session
        .with_settings_manager(|sm| sm.get_string("defaultModel"));
    let reply = control_data(
        &fixture.socket,
        json!({"type": "set_model", "provider": "faux", "modelId": "faux-reasoner"}),
    )
    .await;
    assert_eq!(reply["provider"], "faux");
    assert_eq!(reply["id"], "faux-reasoner");
    assert_eq!(fixture.session.model().id, "faux-reasoner");
    assert_eq!(
        fixture
            .session
            .with_settings_manager(|sm| sm.get_string("defaultModel")),
        default_before
    );
    let entries = fixture
        .session
        .with_session_manager(|manager| manager.entries());
    let model_changes: Vec<_> = entries
        .iter()
        .filter(|e| e["type"] == "model_change")
        .collect();
    assert!(
        !model_changes.is_empty(),
        "a model_change entry was recorded"
    );
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn session_control_set_model_refuses_unknown_model_and_preserves_running_model() {
    let fixture = start_endpoint(None).await;
    let before = fixture.session.model().id.clone();
    let reply = control_request(
        &fixture.socket,
        json!({"type": "set_model", "provider": "faux", "modelId": "no-such"}),
    )
    .await;
    assert_eq!(reply["success"], false);
    assert_eq!(reply["error"], "Model not found: faux/no-such");
    assert_eq!(fixture.session.model().id, before);
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn session_control_set_thinking_level_validates_against_active_model() {
    use maho_ai::providers::faux::FauxModelDefinition;
    let fixture = start_endpoint_with_provider_models(vec![
        FauxModelDefinition {
            id: "faux-reasoner".into(),
            name: Some("Faux Reasoner".into()),
            reasoning: Some(true),
            ..Default::default()
        },
        FauxModelDefinition {
            id: "faux-plain".into(),
            name: Some("Faux Plain".into()),
            ..Default::default()
        },
    ])
    .await;
    let levels = fixture.session.get_available_thinking_levels();
    let supported = levels
        .into_iter()
        .find(|l| *l != fixture.session.thinking_level() && *l != ModelThinkingLevel::Off)
        .expect("supported thinking level on reasoner");
    let reply = control_request(
        &fixture.socket,
        json!({"type": "set_thinking_level", "level": supported.as_str(), "scope": "turn"}),
    )
    .await;
    assert_eq!(reply["success"], true, "{reply}");
    assert_eq!(fixture.session.thinking_level(), supported);

    let switched = control_data(
        &fixture.socket,
        json!({"type": "set_model", "provider": "faux", "modelId": "faux-plain"}),
    )
    .await;
    assert_eq!(switched["id"], "faux-plain");
    let current = fixture.session.thinking_level();

    let refusal = control_request(
        &fixture.socket,
        json!({"type": "set_thinking_level", "level": "high"}),
    )
    .await;
    assert_eq!(refusal["success"], false);
    assert_eq!(
        refusal["error"],
        "Thinking level high is not supported by the active model."
    );
    assert_eq!(fixture.session.thinking_level(), current);
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn session_control_set_thinking_level_remembers_level_without_turn_scope() {
    use maho_ai::providers::faux::FauxModelDefinition;
    let fixture = start_endpoint_with_provider_models(vec![FauxModelDefinition {
        id: "faux-reasoner".into(),
        name: Some("Faux Reasoner".into()),
        reasoning: Some(true),
        ..Default::default()
    }])
    .await;
    let levels: Vec<_> = fixture
        .session
        .get_available_thinking_levels()
        .into_iter()
        .filter(|l| *l != ModelThinkingLevel::Off)
        .collect();
    assert!(levels.len() >= 2, "need at least two thinking levels");
    let first = levels[0];
    let second = levels[1];
    let remembered = fixture
        .session
        .with_settings_manager(|sm| sm.get_string("defaultThinkingLevel"));

    let reply = control_request(
        &fixture.socket,
        json!({"type": "set_thinking_level", "level": first.as_str(), "scope": "turn"}),
    )
    .await;
    assert_eq!(reply["success"], true);
    assert_eq!(fixture.session.thinking_level(), first);
    assert_eq!(
        fixture
            .session
            .with_settings_manager(|sm| sm.get_string("defaultThinkingLevel")),
        remembered
    );

    let reply2 = control_request(
        &fixture.socket,
        json!({"type": "set_thinking_level", "level": second.as_str()}),
    )
    .await;
    assert_eq!(reply2["success"], true);
    assert_eq!(fixture.session.thinking_level(), second);
    assert_eq!(
        fixture
            .session
            .with_settings_manager(|sm| sm.get_string("defaultThinkingLevel")),
        Some(second.as_str().to_owned())
    );
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn session_control_interrupt_idle_session_answers_false() {
    let fixture = start_endpoint(None).await;
    let data = control_data(&fixture.socket, json!({"type": "interrupt"})).await;
    assert_eq!(data, json!({"interrupted": false}));
    assert!(!fixture.session.is_streaming());
    let state = control_data(&fixture.socket, json!({"type": "get_state"})).await;
    assert_eq!(state["turn_epoch"], 0);
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn session_control_interrupt_running_turn_and_stale_turn_id() {
    let fixture = start_endpoint(None).await;
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (released_tx, released_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let released = Arc::new(tokio::sync::Mutex::new(released_rx));
    let started = Arc::new(started_tx);

    let tool = maho_agent::types::AgentTool {
        label: "hold".into(),
        prepare_arguments: None,
        execute: Arc::new(move |_, _, signal, _| {
            let started = started.clone();
            let released = released.clone();
            Box::pin(async move {
                started.send(()).expect("tool start receiver");
                let signal = signal.expect("running tool cancellation signal");
                tokio::select! {
                    _ = signal.cancelled() => {}
                    result = async { released.lock().await.recv().await } => {
                        result.expect("tool release");
                    }
                }
                maho_agent::types::AgentToolResult::text("released")
            })
        }),
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "hold".into(),
            description: "Hold".into(),
            parameters: json!({"type": "object"}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
    };

    fixture.session.agent().set_tools(vec![tool]);

    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message,
        faux_provider, faux_streams,
    };

    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("faux provider");
    provider.set_responses(vec![
        faux_assistant_message(
            vec![ContentBlock::ToolCall(ToolCall {
                id: "call-hold".into(),
                name: "hold".into(),
                arguments: Default::default(),
                ..Default::default()
            })],
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(
            "unreached",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Stop),
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context = context.clone();
        context.system_prompt = context.system_prompt.filter(|p| !p.is_empty());
        streams.stream_simple(model, &context, options.map(|o| o.simple))
    });
    fixture.session.agent().set_stream_function(stream_fn);

    let session = fixture.session.clone();
    let prompt_task =
        tokio::spawn(async move { session.prompt("keep going", Default::default()).await });

    tokio::time::timeout(std::time::Duration::from_secs(10), started_rx.recv())
        .await
        .expect("tool start deadline")
        .expect("hold tool started");
    assert!(fixture.session.is_streaming());

    let state = control_data(&fixture.socket, json!({"type": "get_state"})).await;
    let turn_epoch = state["turn_epoch"].as_u64().unwrap_or(0);
    let turn_id = turn_epoch.to_string();

    let stale = control_data(
        &fixture.socket,
        json!({"type": "interrupt", "turnId": "stale"}),
    )
    .await;
    assert_eq!(
        stale,
        json!({"interrupted": false, "turnId": turn_id.clone()})
    );
    assert!(fixture.session.is_streaming());

    let interrupted = control_data(
        &fixture.socket,
        json!({"type": "interrupt", "turnId": turn_id}),
    )
    .await;
    assert_eq!(interrupted, json!({"interrupted": true, "turnId": turn_id}));

    let _ = released_tx.send(());
    tokio::time::timeout(std::time::Duration::from_secs(10), prompt_task)
        .await
        .expect("interrupted prompt deadline")
        .expect("prompt task")
        .expect("prompt result");

    assert!(!fixture.session.is_streaming());
    let renamed = control_data(
        &fixture.socket,
        json!({"type": "set_session_name", "name": "still-open"}),
    )
    .await;
    assert!(renamed.is_null());
    assert_eq!(
        fixture.session.session_name().as_deref(),
        Some("still-open")
    );
    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn session_status_subscription_and_mirror_content_routing() {
    let fixture = start_endpoint(None).await;
    let mut plain = SocketClient::connect(&fixture.socket).await;
    let mut mirrored = SocketClient::connect(&fixture.socket).await;

    let sub_plain = plain
        .request(json!({"type": "subscribe_sessions", "sessions": "all"}))
        .await;
    assert_eq!(sub_plain["success"], true, "{sub_plain}");
    let snapshot = sub_plain["data"]["snapshot"]
        .as_array()
        .expect("snapshot array");
    assert_eq!(snapshot.len(), 1);
    assert_eq!(snapshot[0]["sessionId"], fixture.session.session_id());
    assert_eq!(snapshot[0]["attachments"], 1);

    let sub_mirrored = mirrored
        .request(json!({
            "type": "subscribe_sessions",
            "sessions": "all",
            "mirror": ["message_update"]
        }))
        .await;
    assert_eq!(sub_mirrored["success"], true, "{sub_mirrored}");

    fixture
        .session
        .prompt("go", Default::default())
        .await
        .expect("prompt ok");

    let delta = mirrored.wait_for(|r| r["type"] == "message_update").await;
    assert_eq!(delta["type"], "message_update");

    let rows = plain
        .request(json!({"type": "subscribe_sessions", "sessions": "all"}))
        .await;
    assert!(
        plain.records.iter().any(|r| r["type"] == "session_status"
            && (r["status"] == "running" || r["status"] == "idle")),
        "plain subscriber should receive actual status transitions"
    );
    assert!(
        plain.records.iter().all(|r| r["type"] != "message_update"),
        "plain subscriber should not receive mirrored message_update content"
    );

    assert_eq!(rows["data"]["snapshot"][0]["attachments"], 1);
    assert_eq!(rows["data"]["snapshot"][0]["status"], "idle");

    fixture.endpoint.dispose().await;
}

#[tokio::test]
async fn live_prompt_and_steer_admission_under_turn_and_idle() {
    let fixture = start_endpoint(None).await;
    let mut client = SocketClient::connect(&fixture.socket).await;
    let (settled_tx, mut settled_rx) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = fixture.session.subscribe(Arc::new(move |event| {
        if matches!(event, maho_ext_api::AgentSessionEvent::AgentSettled) {
            settled_tx.send(()).expect("settled receiver retained");
        }
    }));

    let started = client
        .request(json!({"type": "prompt", "message": "hello from client"}))
        .await;
    assert_eq!(started["success"], true, "{started}");
    assert_eq!(started["data"]["disposition"], "started");

    tokio::time::timeout(std::time::Duration::from_secs(5), settled_rx.recv())
        .await
        .expect("prompt turn deadline")
        .expect("prompt turn finished");

    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let (released_tx, released_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let released = Arc::new(tokio::sync::Mutex::new(released_rx));
    let started_chan = Arc::new(started_tx);

    let tool = maho_agent::types::AgentTool {
        label: "hold".into(),
        prepare_arguments: None,
        execute: Arc::new(move |_, _, _, _| {
            let started = started_chan.clone();
            let released = released.clone();
            Box::pin(async move {
                let _ = started.send(());
                let _ = released.lock().await.recv().await;
                maho_agent::types::AgentToolResult::text("released")
            })
        }),
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "hold".into(),
            description: "Hold".into(),
            parameters: json!({"type": "object"}),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
    };
    fixture.session.agent().set_tools(vec![tool]);

    use maho_ai::providers::faux::{
        FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message,
        faux_provider, faux_streams,
    };

    let provider = faux_provider(RegisterFauxProviderOptions {
        tokens_per_second: Some(0.0),
        ..Default::default()
    })
    .expect("faux provider");
    provider.set_responses(vec![
        faux_assistant_message(
            vec![ContentBlock::ToolCall(ToolCall {
                id: "call-hold".into(),
                name: "hold".into(),
                arguments: Default::default(),
                ..Default::default()
            })],
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
        faux_assistant_message(
            "finished",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Stop),
                timestamp: Some(0),
                ..Default::default()
            },
        )
        .into(),
    ]);
    let streams = faux_streams(provider.core.clone());
    let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| {
        let mut context = context.clone();
        context.system_prompt = context.system_prompt.filter(|p| !p.is_empty());
        streams.stream_simple(model, &context, options.map(|o| o.simple))
    });
    fixture.session.agent().set_stream_function(stream_fn);

    let session = fixture.session.clone();
    let prompt_task =
        tokio::spawn(async move { session.prompt("run turn", Default::default()).await });
    tokio::time::timeout(std::time::Duration::from_secs(10), started_rx.recv())
        .await
        .expect("tool start deadline")
        .expect("tool started");
    assert!(fixture.session.is_streaming());

    let busy = client
        .request(json!({"type": "prompt", "message": "unsupported busy prompt"}))
        .await;
    assert_eq!(busy["success"], false);
    assert_eq!(busy["errorCode"], "session_busy");

    let steered = client
        .request(json!({"type": "steer", "message": "steer while running"}))
        .await;
    assert_eq!(steered["success"], true, "{steered}");
    assert_eq!(steered["data"]["disposition"], "queued");

    let followed = client
        .request(json!({"type": "follow_up", "message": "follow up while running"}))
        .await;
    assert_eq!(followed["success"], true, "{followed}");
    assert_eq!(followed["data"]["disposition"], "queued");

    let _ = released_tx.send(());
    tokio::time::timeout(std::time::Duration::from_secs(10), prompt_task)
        .await
        .expect("queued prompt deadline")
        .expect("prompt task")
        .expect("prompt result");

    *lock(&fixture.surface.draft) = Some("active-draft".into());
    let held = client
        .request(json!({"type": "steer", "message": "steer while draft is held"}))
        .await;
    assert_eq!(held["success"], false);
    assert_eq!(held["errorCode"], "held_draft");

    fixture.endpoint.dispose().await;
}
