//! Behaviour parity tests for the JSON-RPC stdio server.

use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

use mcp_stdio_core::JsonRpcId;
use mcp_stdio_core::JsonRpcResponse;
use mcp_stdio_core::JsonRpcResult;
use mcp_stdio_core::JsonRpcStdioServerConfig;
use mcp_stdio_core::McpLogFields;
use mcp_stdio_core::ParentWatchdogConfig;
use mcp_stdio_core::ProcessLiveness;
use mcp_stdio_core::ServerError;
use mcp_stdio_core::ServerOutcome;
use mcp_stdio_core::classify_probe_error;
use mcp_stdio_core::is_process_alive;
use mcp_stdio_core::json_rpc_id;
use mcp_stdio_core::parent_process_id;
use mcp_stdio_core::run_json_rpc_stdio_server;
use mcp_stdio_core::success_response;

const WAIT: Duration = Duration::from_secs(5);
const ESRCH: i32 = 3;
const EPERM: i32 = 1;
const EINVAL: i32 = 22;

#[test]
fn a_line_request_is_answered_with_a_line_response() {
    let output = SharedWriter::default();
    let mut run = start_server(acknowledging_handler(), output.clone(), |_config| {});

    run.send_line("{\"jsonrpc\":\"2.0\",\"id\":\"ok\",\"method\":\"ping\"}\n");

    assert_eq!(
        output.next_output_line(),
        "{\"jsonrpc\":\"2.0\",\"id\":\"ok\",\"result\":{\"acknowledged\":true}}\n"
    );
    run.close_input();
    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
}

#[test]
fn a_framed_request_is_answered_with_a_framed_response() {
    let output = SharedWriter::default();
    let mut run = start_server(acknowledging_handler(), output.clone(), |_config| {});
    let body = "{\"jsonrpc\":\"2.0\",\"id\":\"framed\",\"method\":\"ping\"}";

    run.send_line(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));

    let response = "{\"jsonrpc\":\"2.0\",\"id\":\"framed\",\"result\":{\"acknowledged\":true}}";
    let expected = format!("Content-Length: {}\r\n\r\n{response}", response.len());
    assert_eq!(output.next_output_bytes(expected.len()), expected);
    run.close_input();
    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
}

#[test]
fn a_parse_error_override_is_written() {
    let output = SharedWriter::default();
    let mut run = start_server(silent_handler(), output.clone(), |config| {
        config.parse_error_response = Some(Arc::new(|_message: &str| {
            Some(JsonRpcResponse::error(
                JsonRpcId::Null,
                -32601,
                "Method not found",
            ))
        }));
    });

    run.send_line("garbage\n");

    assert_eq!(
        output.next_output_line(),
        "{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32601,\"message\":\"Method not found\"}}\n"
    );
    run.close_input();
    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
}

#[test]
fn a_malformed_payload_uses_the_default_parse_error_response() {
    let output = SharedWriter::default();
    let mut run = start_server(silent_handler(), output.clone(), |_config| {});

    run.send_line("garbage\n");

    let parse_message = serde_json::from_str::<Value>("garbage")
        .expect_err("garbage is not JSON")
        .to_string();
    let written: Value =
        serde_json::from_str(&output.next_output_line()).expect("response is JSON");
    assert_eq!(
        written,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32700, "message": "Parse error", "data": parse_message },
        })
    );
    run.close_input();
    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
}

#[test]
fn a_closed_output_settles_the_server_without_an_error() {
    let events = EventLog::default();
    let mut run = start_server(
        acknowledging_handler(),
        failing_writer(std::io::ErrorKind::BrokenPipe),
        {
            let events = events.clone();
            move |config| config.log = Some(Arc::new(events.record()))
        },
    );

    run.send_line("{\"jsonrpc\":\"2.0\",\"id\":\"closed\",\"method\":\"ping\"}\n");

    assert_eq!(run.wait_outcome(), ServerOutcome::TerminalOutputError);
    assert_eq!(
        events.events(),
        vec!["stdio_started", "request", "output_error", "stdio_stopped"]
    );
}

#[test]
fn a_non_terminal_write_failure_is_reported_as_an_error() {
    let mut run = start_server(
        acknowledging_handler(),
        failing_writer(std::io::ErrorKind::Other),
        |_config| {},
    );

    run.send_line("{\"jsonrpc\":\"2.0\",\"id\":\"unknown\",\"method\":\"ping\"}\n");

    match run.wait_error() {
        ServerError::Io(error) => assert_eq!(error.kind(), std::io::ErrorKind::Other),
        ServerError::Handler(error) => panic!("expected an io error, got a handler error: {error}"),
    }
}

#[test]
fn a_missing_watchdog_changes_nothing_and_never_reports_a_parent_exit() {
    let events = EventLog::default();
    let mut run = start_server(silent_handler(), SharedWriter::default(), {
        let events = events.clone();
        move |config| config.log = Some(Arc::new(events.record()))
    });

    run.close_input();

    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
    assert_eq!(events.events(), vec!["stdio_started", "stdio_stopped"]);
}

#[test]
fn an_idle_server_settles_with_an_idle_timeout() {
    let events = EventLog::default();
    let mut run = start_server(silent_handler(), SharedWriter::default(), {
        let events = events.clone();
        move |config| {
            config.idle_timeout_ms = Some(20);
            config.log = Some(Arc::new(events.record()));
        }
    });

    assert_eq!(run.wait_outcome(), ServerOutcome::IdleTimeout);
    assert_eq!(
        events.events(),
        vec!["stdio_started", "idle_timeout", "stdio_stopped"]
    );
}

#[test]
fn a_dead_watched_parent_settles_the_server_and_fires_the_hook() {
    let events = EventLog::default();
    let exits = Arc::new(Mutex::new(0_u32));
    let mut run = start_server(silent_handler(), SharedWriter::default(), {
        let events = events.clone();
        let exits = Arc::clone(&exits);
        move |config| {
            config.parent_watchdog = Some(ParentWatchdogConfig {
                parent_pid: Some(424_242),
                poll_interval_ms: Some(10),
                probe_alive: Some(Arc::new(|_pid| false)),
            });
            config.on_parent_exit = Some(Arc::new(move || {
                *exits.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            }));
            config.log = Some(Arc::new(events.record()));
        }
    });

    assert_eq!(run.wait_outcome(), ServerOutcome::ParentExit);
    assert_eq!(*exits.lock().unwrap_or_else(PoisonError::into_inner), 1);
    assert_eq!(
        events.events(),
        vec!["stdio_started", "parent_exit", "stdio_stopped"]
    );
}

#[test]
fn a_parent_exit_stops_the_poll_after_the_first_report() {
    let events = EventLog::default();
    let mut run = start_server(silent_handler(), SharedWriter::default(), {
        let events = events.clone();
        move |config| {
            config.parent_watchdog = Some(ParentWatchdogConfig {
                parent_pid: Some(424_242),
                poll_interval_ms: Some(10),
                probe_alive: Some(Arc::new(|_pid| false)),
            });
            config.log = Some(Arc::new(events.record()));
        }
    });

    // The server joins the watchdog thread before it returns, so no further poll can run here.
    assert_eq!(run.wait_outcome(), ServerOutcome::ParentExit);
    assert_eq!(
        events
            .events()
            .iter()
            .filter(|event| event.as_str() == "parent_exit")
            .count(),
        1
    );
}

#[test]
fn a_denied_probe_is_treated_as_alive_and_the_server_keeps_serving() {
    let (probe, probed) =
        recording_probe(|_pid| classify_probe_error(Some(EPERM)) == ProcessLiveness::Alive);
    let events = EventLog::default();
    let output = SharedWriter::default();
    let mut run = start_server(acknowledging_handler(), output.clone(), {
        let events = events.clone();
        move |config| {
            config.parent_watchdog = Some(ParentWatchdogConfig {
                parent_pid: Some(424_242),
                poll_interval_ms: Some(10),
                probe_alive: Some(probe),
            });
            config.log = Some(Arc::new(events.record()));
        }
    });

    for _ in 0..3 {
        probed.recv_timeout(WAIT).expect("three liveness polls");
    }
    run.send_line("{\"jsonrpc\":\"2.0\",\"id\":\"alive\",\"method\":\"ping\"}\n");
    assert_eq!(
        output.next_output_line(),
        "{\"jsonrpc\":\"2.0\",\"id\":\"alive\",\"result\":{\"acknowledged\":true}}\n"
    );

    run.close_input();
    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
    assert!(!events.events().iter().any(|event| event == "parent_exit"));
}

#[test]
fn a_configured_parent_pid_is_probed_instead_of_the_current_parent() {
    let (probe, probed) = recording_probe(|_pid| true);
    let mut run = start_server(silent_handler(), SharedWriter::default(), move |config| {
        config.parent_watchdog = Some(ParentWatchdogConfig {
            parent_pid: Some(424_242),
            poll_interval_ms: Some(10),
            probe_alive: Some(probe),
        });
    });

    let first = probed.recv_timeout(WAIT).expect("first liveness poll");
    let second = probed.recv_timeout(WAIT).expect("second liveness poll");

    assert_eq!(first, 424_242);
    assert_eq!(second, 424_242);
    assert!(!next_probes_include(&probed, parent_process_id()));
    run.close_input();
    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
}

#[test]
fn the_current_parent_is_probed_when_no_parent_pid_is_configured() {
    let (probe, probed) = recording_probe(|_pid| true);
    let mut run = start_server(silent_handler(), SharedWriter::default(), move |config| {
        config.parent_watchdog = Some(ParentWatchdogConfig {
            parent_pid: None,
            poll_interval_ms: Some(10),
            probe_alive: Some(probe),
        });
    });

    let first = probed.recv_timeout(WAIT).expect("first liveness poll");
    let second = probed.recv_timeout(WAIT).expect("second liveness poll");

    assert_eq!(first, parent_process_id());
    assert_eq!(second, parent_process_id());
    run.close_input();
    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
}

#[cfg(unix)]
#[test]
fn a_killed_watched_parent_settles_the_server_through_the_real_probe() {
    let mut watched = WatchedParent::spawn();
    let parent_pid = watched.pid();
    let exits = Arc::new(Mutex::new(0_u32));
    let mut run = start_server(silent_handler(), SharedWriter::default(), {
        let exits = Arc::clone(&exits);
        move |config| {
            config.parent_watchdog = Some(ParentWatchdogConfig {
                parent_pid: Some(parent_pid),
                poll_interval_ms: Some(20),
                probe_alive: None,
            });
            config.on_parent_exit = Some(Arc::new(move || {
                *exits.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            }));
        }
    });

    assert!(is_process_alive(parent_pid));
    watched.kill_and_reap();

    assert_eq!(run.wait_outcome(), ServerOutcome::ParentExit);
    assert_eq!(*exits.lock().unwrap_or_else(PoisonError::into_inner), 1);
}

#[cfg(unix)]
#[test]
fn a_live_watched_parent_keeps_the_server_serving() {
    let mut watched = WatchedParent::spawn();
    let parent_pid = watched.pid();
    let output = SharedWriter::default();
    let mut run = start_server(acknowledging_handler(), output.clone(), move |config| {
        config.parent_watchdog = Some(ParentWatchdogConfig {
            parent_pid: Some(parent_pid),
            poll_interval_ms: Some(20),
            probe_alive: None,
        });
    });

    for index in 0..3 {
        run.send_line(&format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":\"qa-{index}\",\"method\":\"ping\"}}\n"
        ));
        assert_eq!(
            output.next_output_line(),
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":\"qa-{index}\",\"result\":{{\"acknowledged\":true}}}}\n"
            )
        );
    }
    assert!(is_process_alive(parent_pid));

    run.close_input();
    assert_eq!(run.wait_outcome(), ServerOutcome::InputClosed);
    watched.kill_and_reap();
}

#[test]
fn a_running_process_is_reported_alive() {
    assert!(is_process_alive(std::process::id()));
}

#[test]
fn an_esrch_probe_error_is_reported_dead() {
    assert_eq!(classify_probe_error(Some(ESRCH)), ProcessLiveness::Dead);
}

#[test]
fn an_eperm_probe_error_is_reported_alive() {
    assert_eq!(classify_probe_error(Some(EPERM)), ProcessLiveness::Alive);
}

#[test]
fn an_unknown_probe_error_is_reported_alive() {
    assert_eq!(classify_probe_error(Some(EINVAL)), ProcessLiveness::Alive);
}

type TestHandler = Box<dyn FnMut(&Value) -> Result<Option<JsonRpcResponse>, std::io::Error> + Send>;
type TestOutcome = Result<ServerOutcome, ServerError<std::io::Error>>;

struct ServerRun {
    input: Option<Sender<Vec<u8>>>,
    outcomes: Receiver<TestOutcome>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ServerRun {
    fn send_line(&self, line: &str) {
        self.input
            .as_ref()
            .expect("input still open")
            .send(line.as_bytes().to_vec())
            .expect("reader thread still reading");
    }

    fn close_input(&mut self) {
        self.input = None;
    }

    fn wait_outcome(&mut self) -> ServerOutcome {
        match self.receive() {
            Ok(outcome) => outcome,
            Err(error) => panic!("server failed: {error}"),
        }
    }

    fn wait_error(&mut self) -> ServerError<std::io::Error> {
        match self.receive() {
            Ok(outcome) => panic!("server settled with {outcome:?}"),
            Err(error) => error,
        }
    }

    fn receive(&mut self) -> TestOutcome {
        match self.outcomes.recv_timeout(WAIT) {
            Ok(outcome) => outcome,
            Err(error) => panic!("server did not settle within {WAIT:?}: {error}"),
        }
    }
}

impl Drop for ServerRun {
    fn drop(&mut self) {
        self.input = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn start_server<W: Write + Send + 'static>(
    handler: TestHandler,
    output: W,
    configure: impl FnOnce(&mut JsonRpcStdioServerConfig<TestHandler, std::io::Error>),
) -> ServerRun {
    let (input_sender, input_receiver) = std::sync::mpsc::channel();
    let (outcome_sender, outcomes) = std::sync::mpsc::channel();
    let mut config = JsonRpcStdioServerConfig::new(handler);
    config.idle_timeout_ms = Some(0);
    configure(&mut config);
    let handle = std::thread::spawn(move || {
        let mut output = output;
        let outcome = run_json_rpc_stdio_server(
            ChannelInput {
                receiver: input_receiver,
            },
            &mut output,
            config,
        );
        let _ = outcome_sender.send(outcome);
    });
    ServerRun {
        input: Some(input_sender),
        outcomes,
        handle: Some(handle),
    }
}

fn acknowledging_handler() -> TestHandler {
    Box::new(|payload: &Value| {
        let mut result = JsonRpcResult::new();
        result.insert("acknowledged".to_string(), Value::Bool(true));
        Ok(Some(success_response(
            json_rpc_id(payload.get("id").unwrap_or(&Value::Null)),
            result,
        )))
    })
}

fn silent_handler() -> TestHandler {
    Box::new(|_payload: &Value| Ok(None))
}

fn failing_writer(kind: std::io::ErrorKind) -> FailingWriter {
    FailingWriter { kind }
}

struct FailingWriter {
    kind: std::io::ErrorKind,
}

impl Write for FailingWriter {
    fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::new(self.kind, "synthetic output failure"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct ChannelInput {
    receiver: Receiver<Vec<u8>>,
}

impl Read for ChannelInput {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self.receiver.recv() {
            Ok(chunk) => {
                let copied = chunk.len().min(buffer.len());
                buffer[..copied].copy_from_slice(&chunk[..copied]);
                Ok(copied)
            }
            Err(_) => Ok(0),
        }
    }
}

#[derive(Clone, Default)]
struct SharedWriter {
    state: Arc<WriterState>,
}

#[derive(Default)]
struct WriterState {
    buffer: Mutex<Vec<u8>>,
    changed: Condvar,
}

impl SharedWriter {
    fn next_output_line(&self) -> String {
        self.next_output(|buffer| {
            buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
        })
    }

    fn next_output_bytes(&self, length: usize) -> String {
        self.next_output(|buffer| (buffer.len() >= length).then_some(length))
    }

    /// Waits until `complete` reports how many buffered bytes form the next output unit.
    fn next_output(&self, complete: impl Fn(&[u8]) -> Option<usize>) -> String {
        let deadline = Instant::now() + WAIT;
        let mut buffer = self
            .state
            .buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(length) = complete(&buffer) {
                let unit = String::from_utf8_lossy(&buffer[..length]).to_string();
                buffer.drain(..length);
                return unit;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "no output within {WAIT:?}");
            let (guard, _timeout) = self
                .state
                .changed
                .wait_timeout(buffer, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            buffer = guard;
        }
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let mut pending = self
            .state
            .buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        pending.extend_from_slice(buffer);
        drop(pending);
        self.state.changed.notify_all();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct EventLog(Arc<Mutex<Vec<String>>>);

impl EventLog {
    fn record(&self) -> impl Fn(&str, Option<&McpLogFields>) + Send + Sync + 'static {
        let events = Arc::clone(&self.0);
        move |event, _fields| {
            events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(event.to_string());
        }
    }

    fn events(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn recording_probe(
    verdict: impl Fn(u32) -> bool + Send + Sync + 'static,
) -> (Arc<dyn Fn(u32) -> bool + Send + Sync>, Receiver<u32>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let sender = Mutex::new(sender);
    let probe = Arc::new(move |pid: u32| {
        let _ = sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(pid);
        verdict(pid)
    });
    (probe, receiver)
}

/// Inspects the next three polls; the watchdog probes every tick, so three samples are enough.
fn next_probes_include(probed: &Receiver<u32>, pid: u32) -> bool {
    (0..3).any(|_| probed.recv_timeout(WAIT).expect("another liveness poll") == pid)
}

#[cfg(unix)]
struct WatchedParent {
    child: std::process::Child,
}

#[cfg(unix)]
impl WatchedParent {
    fn spawn() -> Self {
        Self {
            child: std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn a watched parent"),
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn kill_and_reap(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
impl Drop for WatchedParent {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}
