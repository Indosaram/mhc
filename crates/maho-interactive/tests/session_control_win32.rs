//! Port of senpi test/suite/interactive-session-control-win32.test.ts (senpi#4267, senpi-interactive-30):
//! on win32 a terminal registers its control endpoint at the logical `<agentDir>/rpc/tui/t-*.sock` path,
//! the 32-byte secret sits beside it, and the listener binds the named pipe the host transport derives
//! from both. The endpoint is a live `tui` row of `host status --all`, and a client that sent the secret
//! declares the user's role, attaches on the session's identity, prompts, reads the turn's records and
//! stops the session over the pipe.
use maho_core::extensions::session_control_types::{
    RegisterControlEndpointOptions, SessionControlRegistration,
};
use maho_ext_api::QuestionResponse;
use maho_interactive::session_control_commands::TuiControlSurface;
use maho_interactive::session_control_endpoint::{
    ControlEndpointOutcome, register_session_control_endpoint,
};
use maho_interactive::session_control_lifecycle::TuiControlContext;
use maho_interactive::session_control_session_commands::TuiSessionControls;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

struct Surface;
impl TuiSessionControls for Surface {
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
impl TuiControlSurface for Surface {
    fn draft_hold(&self) -> Option<String> {
        None
    }
    fn blocking_question(&self) -> bool {
        false
    }
    fn pending_question_ids(&self) -> Vec<String> {
        Vec::new()
    }
    fn pending_question(&self, _request_id: &str) -> Option<Vec<Value>> {
        None
    }
    fn answer_question(&self, _request_id: &str, _response: QuestionResponse) -> bool {
        false
    }
    fn notice(&self, _line: &str) {}
}

/// One connection over the endpoint's named pipe: every record it reads, in order.
struct PipeClient {
    write: tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>,
    lines: tokio::io::Lines<
        tokio::io::BufReader<tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>>,
    >,
    records: Vec<Value>,
    next: u32,
}
impl PipeClient {
    async fn connect(logical_path: &str) -> Self {
        let secret = std::fs::read(
            maho_rpc::socket_transport::socket_secret_path(logical_path).expect("secret path"),
        )
        .expect("secret");
        let address = maho_rpc::socket_transport::resolve_socket_transport_address(
            logical_path,
            "win32",
            Some(&secret),
        )
        .expect("pipe address");
        assert!(address.starts_with("\\\\.\\pipe\\"), "{address}");
        let pipe = maho_rpc::host_lifecycle::win32_pipe::connect(
            &address,
            std::time::Duration::from_secs(10),
        )
        .await
        .expect("the pipe accepts");
        let (read, mut write) = tokio::io::split(pipe);
        write.write_all(&secret).await.expect("secret sent");
        Self {
            write,
            lines: tokio::io::BufReader::new(read).lines(),
            records: Vec::new(),
            next: 0,
        }
    }
    /// The first record matching `matches`, reading on from what was already seen.
    async fn wait_for(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        if let Some(seen) = self.records.iter().find(|record| matches(record)) {
            return seen.clone();
        }
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let line = self
                    .lines
                    .next_line()
                    .await
                    .expect("pipe read")
                    .expect("the endpoint keeps the pipe open");
                let Ok(record) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                self.records.push(record.clone());
                if matches(&record) {
                    return record;
                }
            }
        })
        .await
        .expect("the record arrives")
    }
    async fn request(&mut self, mut command: Value) -> Value {
        self.next += 1;
        let id = format!("r{}", self.next);
        command["id"] = json!(id);
        self.write
            .write_all(format!("{command}\n").as_bytes())
            .await
            .expect("request sent");
        self.wait_for(|record| record["type"] == "response" && record["id"] == id.as_str())
            .await
    }
}

#[tokio::test]
async fn registers_over_a_named_pipe_is_listed_by_host_status_all_and_is_attached_and_driven() {
    let (_mode, directory, session) =
        crate::runtime35_native::native_mode_with_session_and_provider_retaining_session(
            None,
            |cwd| {
                let sessions = format!("{cwd}/sessions");
                maho_core::session_manager::SessionManager::create(cwd, Some(&sessions), None)
            },
            "faux",
            "faux-1",
            false,
        );
    let agent_dir = directory.path().join("agent");
    let options = RegisterControlEndpointOptions {
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
    let context = TuiControlContext {
        session: session.clone(),
        agent_dir: agent_dir.to_string_lossy().into_owned(),
        surface: Arc::new(Surface),
        editor_revision: Arc::new(|| 0),
    };
    let ControlEndpointOutcome::Registered {
        registration,
        endpoint,
    } = register_session_control_endpoint(context, options).await
    else {
        panic!("the endpoint registers on win32");
    };
    let SessionControlRegistration::Registered { socket, .. } = registration else {
        panic!("a registered outcome carries a registered registration");
    };
    // The logical path never moves to a short fallback on win32.
    assert!(
        std::path::Path::new(&socket).starts_with(agent_dir.join("rpc").join("tui")),
        "{socket}"
    );

    let rows = maho_rpc::host_status_all::read_all_host_status(&agent_dir, false, None)
        .await
        .expect("status rows");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["endpoint_kind"], "tui");
    assert_eq!(rows[0]["alive"], true);

    let mut client = PipeClient::connect(&socket).await;
    let session_id = session.session_id();
    let info = client
        .request(json!({"type": "get_protocol_info", "observe": true}))
        .await;
    let declared = client
        .request(
            json!({"type": "set_client_info", "width": 80, "capabilities": ["interactive_user"]}),
        )
        .await;
    let attached = client
        .request(json!({"type": "attach_session", "sessionId": session_id, "durableSessionId": session_id}))
        .await;
    // The fixture's faux provider answers the first turn with "hello".
    let prompted = client
        .request(json!({"type": "prompt", "message": "hello", "clientMessageId": "w-1"}))
        .await;
    client
        .wait_for(|record| record["type"] == "agent_end")
        .await;
    let stopped = client
        .request(json!({"type": "abort", "intent": "stop"}))
        .await;

    assert!(
        info["data"]["capabilities"]
            .as_array()
            .is_some_and(|capabilities| capabilities.iter().any(|value| value == "tui_observer")),
        "{info}"
    );
    assert_eq!(declared["success"], true, "{declared}");
    assert_eq!(attached["success"], true, "{attached}");
    assert_eq!(
        attached["data"],
        json!({"sessionId": session_id, "attached": true})
    );
    assert_eq!(prompted["success"], true, "{prompted}");
    assert_eq!(prompted["data"]["disposition"], "started");
    assert_eq!(prompted["data"]["clientMessageId"], "w-1");
    assert!(
        client
            .records
            .iter()
            .any(|record| record["type"] == "message_update")
    );
    assert_eq!(stopped["success"], true, "{stopped}");
    assert_eq!(stopped["command"], "abort");
    assert!(!session.is_streaming());
    endpoint.dispose().await;
}
