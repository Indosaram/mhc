//! Port of senpi test/interactive-session-control.test.ts, test/suite/session-control-ui-response.test.ts,
//! test/suite/regressions/4267-observing-reads-terminal-parity.test.ts (the terminal half) and
//! test/suite/regressions/3246-rpc-drain-notice-cap.test.ts (the terminal half), v2026.10.27-2.
//!
//! Each test binds a real endpoint on a real Unix socket over a real session and speaks to it the way
//! a client does: the 32-byte secret first, then one JSONL request per connection.

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
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct TestSurface {
    draft: Mutex<Option<String>>,
    notices: Mutex<Vec<String>>,
    pending: Mutex<Vec<String>>,
    answered: Mutex<Vec<(String, QuestionResponse)>>,
}

impl TuiSessionControls for TestSurface {
    fn select_model(
        &self,
        _model: maho_ai::types::Model,
    ) -> futures::future::BoxFuture<'static, Result<Option<String>, String>> {
        Box::pin(async { Ok(None) })
    }
    fn select_thinking_level(&self, _level: maho_ai::types::ModelThinkingLevel, _remember: bool) {}
    fn interrupt_turn(&self) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(async {})
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
    let (mode, directory, session) =
        crate::runtime35_native::native_mode_with_session_and_provider_retaining_session(
            None,
            |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
            "faux",
            "faux-1",
            false,
        );
    // A short agent directory keeps the socket under `<agentDir>/rpc/tui` within `sun_path`.
    let agent_dir = directory.path().join("a");
    let inbox_dir = directory.path().join("inbox");
    let surface = Arc::new(TestSurface::default());
    let wakes = Arc::new(Mutex::new(Vec::new()));
    let seen = wakes.clone();
    let drain = drain.unwrap_or_else(|| {
        Arc::new(
            |_event: Value| -> futures::future::BoxFuture<'static, Option<Value>> {
                Box::pin(async { None })
            },
        )
    });
    let options = RegisterControlEndpointOptions {
        inbox_dir: inbox_dir.to_string_lossy().into_owned(),
        drain: Arc::new(
            move |event: Value| -> futures::future::BoxFuture<'static, Option<Value>> {
                lock(&seen).push(event.clone());
                drain(event)
            },
        ),
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
        |event: Value| -> futures::future::BoxFuture<'static, Option<Value>> {
            let admitted = event["delivery_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&json!("d2")));
            Box::pin(async move {
                admitted.then(|| json!({"admitted": [{"delivery_id": "d2", "kind": "started"}]}))
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
        move |event: Value| -> futures::future::BoxFuture<'static, Option<Value>> {
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
            Box::pin(async { None })
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
        move |_event: Value| -> futures::future::BoxFuture<'static, Option<Value>> {
            let failing = drain_failing.load(std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                assert!(!failing, "inbox store unreadable");
                None
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
