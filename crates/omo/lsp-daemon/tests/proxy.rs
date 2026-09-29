//! Ports of proxy.test.ts, proxy-protocol-pin.test.ts, proxy-lifecycle.test.ts and
//! proxy-startup-watchdog.test.ts.

mod common;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Reply, context_for, short_tempdir, spawn_fake_daemon, test_paths, write_token};
use lsp_daemon::daemon_client::EnsureFn;
use lsp_daemon::daemon_server::{DaemonServerOptions, start_daemon_server};
use lsp_daemon::proxy::{
    ProxyOptions, StderrSink, infer_open_code_project_cwd, run_mcp_stdio_proxy,
};
use mcp_stdio_core::{ParentWatchdogConfig, ServerOutcome};
use serde_json::{Value, json};

fn lines(messages: &[Value]) -> Vec<u8> {
    messages
        .iter()
        .map(|message| format!("{message}\n"))
        .collect::<String>()
        .into_bytes()
}

fn parse_responses(output: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(output)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn inert_options(root: &tempfile::TempDir) -> ProxyOptions {
    ProxyOptions {
        paths: Some(test_paths(root)),
        context: Some(context_for(root.path())),
        ensure: Some(common::noop_ensure()),
        startup_timeout_ms: Some(0),
        request_timeout_ms: Some(500),
        ..ProxyOptions::default()
    }
}

/// Runs the proxy on a dedicated thread (it owns a blocking runtime).
fn proxy(input: impl Read + Send + 'static, options: ProxyOptions) -> (ServerOutcome, Vec<u8>) {
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let outcome = run_mcp_stdio_proxy(input, &mut output, options).unwrap();
        (outcome, output)
    })
    .join()
    .unwrap()
}

/// Readable that yields scripted chunks and then blocks until released.
struct ScriptedInput {
    chunks: std::collections::VecDeque<Vec<u8>>,
    release: mpsc::Receiver<()>,
    destroyed: Arc<AtomicBool>,
}

impl Read for ScriptedInput {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if let Some(chunk) = self.chunks.pop_front() {
            buffer[..chunk.len()].copy_from_slice(&chunk);
            return Ok(chunk.len());
        }
        let _released = self.release.recv();
        self.destroyed.store(true, Ordering::SeqCst);
        Ok(0)
    }
}

fn scripted(chunks: Vec<Vec<u8>>) -> (ScriptedInput, mpsc::Sender<()>) {
    let (sender, release) = mpsc::channel();
    let input = ScriptedInput {
        chunks: chunks.into(),
        release,
        destroyed: Arc::default(),
    };
    (input, sender)
}

/// Output sink that forwards every write so tests can wait for responses.
struct ChannelOutput(mpsc::Sender<Vec<u8>>);

impl Write for ChannelOutput {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let _sent = self.0.send(buffer.to_vec());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Keeps stdin open (as a real MCP parent does) until `expected` responses arrived,
/// then closes it; closing earlier would cancel the in-flight calls by design.
fn proxy_until_responses(messages: &[Value], expected: usize, options: ProxyOptions) -> Vec<Value> {
    let (input, release) = scripted(vec![lines(messages)]);
    let (output_tx, output_rx) = mpsc::channel();
    let running = std::thread::spawn(move || {
        let mut output = ChannelOutput(output_tx);
        run_mcp_stdio_proxy(input, &mut output, options).unwrap()
    });
    let mut collected = Vec::new();
    while parse_responses(&collected).len() < expected {
        collected.extend(output_rx.recv_timeout(common::TIMEOUT).unwrap());
    }
    release.send(()).unwrap();
    running.join().unwrap();
    parse_responses(&collected)
}

fn stderr_sink() -> (StderrSink, Arc<Mutex<String>>) {
    let collected: Arc<Mutex<String>> = Arc::default();
    let sink: StderrSink = Arc::new({
        let collected = Arc::clone(&collected);
        move |text: &str| {
            collected.lock().unwrap().push_str(text);
            Ok(())
        }
    });
    (sink, collected)
}

#[test]
fn initialize_is_local_and_tools_call_goes_to_the_daemon() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime
        .block_on(start_daemon_server(
            &paths,
            DaemonServerOptions {
                on_idle_shutdown: Some(Arc::new(|| {})),
                skip_version_reap: true,
                ..DaemonServerOptions::default()
            },
        ))
        .unwrap();
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
    ];
    let responses = proxy_until_responses(&messages, 2, inert_options(&root));
    assert!(responses[0]["result"]["serverInfo"].is_object());
    let text = responses[1]["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains("Configured LSP servers"), "{text}");
    runtime.block_on(server.close());
}

#[test]
fn opencode_env_produces_the_env_derived_typed_context() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    write_token(&paths, "env-token");
    let project = tempfile::tempdir().unwrap();
    let project_dir = std::fs::canonicalize(project.path()).unwrap();
    std::fs::create_dir_all(project_dir.join(".opencode")).unwrap();
    let config = project_dir.join(".opencode/lsp.json");
    std::fs::write(&config, "{}").unwrap();
    let user = project_dir.join("user.json");
    let decisions = project_dir.join("decisions.json");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fake = runtime.block_on(async {
        spawn_fake_daemon(
            &paths.socket,
            Arc::new(|message, _index| Reply::Respond(common::tool_result(message, "ok"))),
        )
    });
    let env: HashMap<String, String> = [
        (
            "LSP_TOOLS_MCP_PROJECT_CONFIG",
            config.to_string_lossy().into_owned(),
        ),
        (
            "LSP_TOOLS_MCP_USER_CONFIG",
            user.to_string_lossy().into_owned(),
        ),
        (
            "LSP_TOOLS_MCP_INSTALL_DECISIONS",
            decisions.to_string_lossy().into_owned(),
        ),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect();
    let options = ProxyOptions {
        context: None,
        env: Some(env),
        home_dir: Some(project_dir.to_string_lossy().into_owned()),
        ..inert_options(&root)
    };
    let messages = [
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
    ];
    assert_eq!(
        proxy_until_responses(&messages, 1, options)[0]["result"]["content"][0]["text"],
        json!("ok")
    );
    let request = fake
        .received()
        .into_iter()
        .find(|m| m["method"] == json!("tools/call"))
        .unwrap();
    let context = &request["params"]["arguments"]["_context"];
    assert_eq!(context["cwd"], json!(project_dir.to_string_lossy()));
    assert_eq!(
        context["projectConfigPaths"],
        json!([config.to_string_lossy()])
    );
    assert_eq!(context["userConfigPath"], json!(user.to_string_lossy()));
    assert_eq!(
        context["installDecisionsPath"],
        json!(decisions.to_string_lossy())
    );
    drop(runtime);
}

#[test]
fn unreachable_daemon_tool_gets_structured_error_not_local_execution() {
    let root = short_tempdir();
    let messages = [
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
    ];
    let responses = proxy_until_responses(&messages, 1, inert_options(&root));
    let result = &responses[0]["result"];
    assert_eq!(result["isError"], json!(true));
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("LSP daemon unreachable"), "{text}");
    assert!(!text.contains("Configured LSP servers"));
}

#[test]
fn malformed_line_returns_parse_error() {
    let root = short_tempdir();
    let (_outcome, output) = proxy(
        std::io::Cursor::new(b"not json\n".to_vec()),
        inert_options(&root),
    );
    assert_eq!(parse_responses(&output)[0]["error"]["code"], json!(-32700));
}

#[test]
fn idle_proxy_keeps_serving_after_a_long_pause() {
    let root = short_tempdir();
    let (sender, receiver) = mpsc::channel::<Vec<u8>>();
    struct ChannelInput(mpsc::Receiver<Vec<u8>>);
    impl Read for ChannelInput {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            match self.0.recv() {
                Ok(chunk) => {
                    buffer[..chunk.len()].copy_from_slice(&chunk);
                    Ok(chunk.len())
                }
                Err(_) => Ok(0),
            }
        }
    }
    let running = std::thread::spawn({
        let options = inert_options(&root);
        move || {
            let mut output = Vec::new();
            run_mcp_stdio_proxy(ChannelInput(receiver), &mut output, options).unwrap();
            output
        }
    });
    let initialize = |id: u64| {
        lines(&[json!({"jsonrpc": "2.0", "id": id, "method": "initialize", "params": {}})])
    };
    sender.send(initialize(1)).unwrap();
    // idle_timeout_ms is 0 (disabled), so no idle clock can end the proxy between requests.
    sender.send(initialize(2)).unwrap();
    drop(sender);
    let responses = parse_responses(&running.join().unwrap());
    assert!(responses[0]["result"]["serverInfo"].is_object());
    assert!(responses[1]["result"]["serverInfo"].is_object());
}

#[test]
fn several_unreachable_tools_each_get_errors_and_proxy_keeps_serving() {
    let root = short_tempdir();
    let messages = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "diagnostics", "arguments": {"filePath": "/a.ts"}}}),
    ];
    let responses = proxy_until_responses(&messages, 2, inert_options(&root));
    assert_eq!(responses.len(), 2);
    for response in &responses {
        assert_eq!(response["result"]["isError"], json!(true));
        assert!(
            response["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("LSP daemon unreachable")
        );
    }
}

#[test]
fn initialize_server_info_and_capabilities_are_pinned() {
    let root = short_tempdir();
    let input = lines(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}),
    ]);
    let (_outcome, output) = proxy(std::io::Cursor::new(input), inert_options(&root));
    assert_eq!(
        parse_responses(&output),
        vec![json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "lsp", "version": "0.1.0"},
                "protocolVersion": "2024-11-05",
            },
        })]
    );
}

#[test]
fn malformed_line_parse_error_envelope_includes_parser_data() {
    let root = short_tempdir();
    let (_outcome, output) = proxy(
        std::io::Cursor::new(b"garbage\n".to_vec()),
        inert_options(&root),
    );
    let responses = parse_responses(&output);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["jsonrpc"], json!("2.0"));
    assert_eq!(responses[0]["id"], Value::Null);
    assert_eq!(responses[0]["error"]["code"], json!(-32700));
    assert_eq!(responses[0]["error"]["message"], json!("Parse error"));
    assert!(
        responses[0]["error"]["data"]
            .as_str()
            .unwrap()
            .contains("garbage")
    );
}

#[test]
fn parent_input_close_cancels_in_flight_request_and_closes_its_socket() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    write_token(&paths, "lifecycle-token");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fake = runtime.block_on(async {
        spawn_fake_daemon(&paths.socket, Arc::new(|_message, _index| Reply::Hold))
    });
    let request = lines(&[
        json!({"jsonrpc": "2.0", "id": 42, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
    ]);
    let (input, release) = scripted(vec![request]);
    let options = ProxyOptions {
        request_timeout_ms: Some(30_000),
        ..inert_options(&root)
    };
    let running = std::thread::spawn(move || {
        let mut output = Vec::new();
        run_mcp_stdio_proxy(input, &mut output, options).unwrap();
        output
    });
    runtime.block_on(fake.wait_until(|fake| fake.tool_calls() == 1));
    release.send(()).unwrap();
    let output = running.join().unwrap();
    let response = &parse_responses(&output)[0];
    assert_eq!(response["id"], json!(42));
    assert_eq!(response["result"]["isError"], json!(true));
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cancelled")
    );
    runtime.block_on(fake.wait_until(|fake| fake.closed() == 1));
    assert!(
        fake.received()
            .iter()
            .any(|m| m["method"] == json!("$/cancelRequest"))
    );
    assert_eq!(fake.tool_calls(), 1);
}

#[test]
fn input_end_during_pending_startup_cancels_without_retrying() {
    let root = short_tempdir();
    let attempts = Arc::new(AtomicUsize::new(0));
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let started_tx = Arc::new(Mutex::new(started_tx));
    let ensure: EnsureFn = Arc::new({
        let attempts = Arc::clone(&attempts);
        move |_paths, _signal| {
            attempts.fetch_add(1, Ordering::SeqCst);
            let _sent = started_tx.lock().unwrap().send(());
            Box::pin(std::future::pending())
        }
    });
    let request = lines(&[
        json!({"jsonrpc": "2.0", "id": 42, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
    ]);
    let (input, release) = scripted(vec![request]);
    let options = ProxyOptions {
        ensure: Some(ensure),
        ..inert_options(&root)
    };
    let running = std::thread::spawn(move || {
        let mut output = Vec::new();
        run_mcp_stdio_proxy(input, &mut output, options).unwrap();
        output
    });
    started_rx.recv_timeout(common::TIMEOUT).unwrap();
    release.send(()).unwrap();
    let output = running.join().unwrap();
    let response = &parse_responses(&output)[0];
    assert_eq!(response["id"], json!(42));
    assert_eq!(response["result"]["isError"], json!(true));
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cancelled")
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn parent_death_exits_the_proxy_while_stdin_stays_open() {
    let root = short_tempdir();
    let initialize =
        lines(&[json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})]);
    let (input, _release) = scripted(vec![initialize]);
    let parent_alive = Arc::new(AtomicBool::new(true));
    let options = ProxyOptions {
        parent_watchdog: Some(ParentWatchdogConfig {
            parent_pid: Some(4242),
            poll_interval_ms: Some(5),
            probe_alive: Some(Arc::new({
                let parent_alive = Arc::clone(&parent_alive);
                move |_pid| parent_alive.load(Ordering::SeqCst)
            })),
        }),
        ..inert_options(&root)
    };
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let outcome = run_mcp_stdio_proxy(input, &mut output, options).unwrap();
        let _sent = done_tx.send((outcome, output));
    });
    parent_alive.store(false, Ordering::SeqCst);
    let (outcome, output) = done_rx.recv_timeout(common::TIMEOUT).unwrap();
    assert_eq!(outcome, ServerOutcome::ParentExit);
    assert!(parse_responses(&output)[0]["result"]["serverInfo"].is_object());
}

#[test]
fn synchronous_setup_failure_rejects_without_leaking_lifecycle_state() {
    let root = short_tempdir();
    let options = ProxyOptions {
        context: None,
        cwd: Some(
            root.path()
                .join("missing-cwd")
                .to_string_lossy()
                .into_owned(),
        ),
        env: Some(HashMap::new()),
        ..inert_options(&root)
    };
    let (input, _release) = scripted(Vec::new());
    let destroyed = Arc::clone(&input.destroyed);
    let mut output = Vec::new();
    assert!(run_mcp_stdio_proxy(input, &mut output, options).is_err());
    assert!(output.is_empty());
    assert!(!destroyed.load(Ordering::SeqCst));
}

fn watchdog_options(root: &tempfile::TempDir, stderr: StderrSink) -> ProxyOptions {
    ProxyOptions {
        startup_timeout_ms: Some(20),
        stderr: Some(stderr),
        ..inert_options(root)
    }
}

#[test]
fn silent_parent_startup_expiry_exits_the_proxy() {
    let root = short_tempdir();
    let (input, _release) = scripted(Vec::new());
    let (sink, collected) = stderr_sink();
    let (outcome, _output) = proxy(input, watchdog_options(&root, sink));
    assert_eq!(outcome, ServerOutcome::InputClosed);
    assert!(
        collected
            .lock()
            .unwrap()
            .contains("no MCP request received within 20ms")
    );
}

#[test]
fn partial_frame_does_not_disable_the_startup_watchdog() {
    let root = short_tempdir();
    let (input, _release) = scripted(vec![br#"{"jsonrpc":"2.0","id":1,"#.to_vec()]);
    let (sink, collected) = stderr_sink();
    let (_outcome, output) = proxy(input, watchdog_options(&root, sink));
    assert!(
        collected
            .lock()
            .unwrap()
            .contains("no MCP request received within 20ms")
    );
    assert!(!String::from_utf8_lossy(&output).contains("\"id\":1,\"result\""));
}

#[test]
fn throwing_diagnostic_stream_cannot_prevent_shutdown() {
    let root = short_tempdir();
    let (input, _release) = scripted(Vec::new());
    let sink: StderrSink = Arc::new(|_text: &str| Err(std::io::Error::other("stderr exploded")));
    let (outcome, _output) = proxy(input, watchdog_options(&root, sink));
    assert_eq!(outcome, ServerOutcome::InputClosed);
}

#[test]
fn first_request_before_expiry_disables_the_startup_watchdog() {
    let root = short_tempdir();
    let initialize =
        lines(&[json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})]);
    let (input, release) = scripted(vec![initialize]);
    let destroyed = Arc::clone(&input.destroyed);
    let (sink, collected) = stderr_sink();
    let (output_tx, output_rx) = mpsc::channel();
    let options = watchdog_options(&root, sink);
    let running = std::thread::spawn(move || {
        let mut output = ChannelOutput(output_tx);
        run_mcp_stdio_proxy(input, &mut output, options).unwrap()
    });
    let first = output_rx.recv_timeout(common::TIMEOUT).unwrap();
    assert!(String::from_utf8_lossy(&first).contains("\"id\":1"));
    // Well past the 20ms startup window: the proxy must still be running.
    assert!(output_rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert!(!running.is_finished());
    assert!(!destroyed.load(Ordering::SeqCst));
    assert!(collected.lock().unwrap().is_empty());
    release.send(()).unwrap();
    assert_eq!(running.join().unwrap(), ServerOutcome::InputClosed);
}

#[test]
fn opencode_config_paths_infer_the_project_cwd() {
    assert_eq!(
        infer_open_code_project_cwd(Some("/work/app/.opencode/lsp.json")),
        Some("/work/app".to_string())
    );
    assert_eq!(
        infer_open_code_project_cwd(Some("/x/y.json:/work/app/.omo/lsp-client.json")),
        Some("/work/app".to_string())
    );
    assert_eq!(
        infer_open_code_project_cwd(Some("/work/app/lsp.json")),
        None
    );
    assert_eq!(infer_open_code_project_cwd(None), None);
}
