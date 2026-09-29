use std::collections::BTreeSet;
use std::io::Cursor;
use std::io::Read;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use ast_grep_mcp::AST_GREP_ERROR_CODES;
use ast_grep_mcp::AST_GREP_MCP_NAME;
use ast_grep_mcp::AbortSignal;
use ast_grep_mcp::AstGrepMcpOptions;
use ast_grep_mcp::AstGrepToolExecutors;
use ast_grep_mcp::ast_grep_mcp_tools;
use ast_grep_mcp::handle_ast_grep_mcp_request;
use ast_grep_mcp::mcp::SgResolveError;
use ast_grep_mcp::mcp::ToolExecutor;
use ast_grep_mcp::run_mcp_stdio_server;
use mcp_stdio_core::McpLogFields;
use mcp_stdio_core::ParentWatchdogConfig;
use mcp_stdio_core::ServerOutcome;
use pretty_assertions::assert_eq;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

const SG_PATH: &str = "/stub/sg";
const BOUND: Duration = Duration::from_secs(5);

fn stub_options() -> AstGrepMcpOptions {
    AstGrepMcpOptions {
        resolve_sg_path: Some(Arc::new(|| Ok(SG_PATH.to_owned()))),
        ..AstGrepMcpOptions::default()
    }
}

fn with_executors(executors: AstGrepToolExecutors) -> AstGrepMcpOptions {
    AstGrepMcpOptions {
        executors,
        ..stub_options()
    }
}

fn handle(request: Value, options: &AstGrepMcpOptions) -> Option<Value> {
    handle_ast_grep_mcp_request(&request, options)
        .map(|response| serde_json::to_value(response).expect("serialize response"))
}

fn handle_default(request: Value) -> Value {
    handle(request, &stub_options()).expect("response")
}

fn call(name: &str, arguments: Value, options: &AstGrepMcpOptions) -> Value {
    handle(
        json!({ "jsonrpc": "2.0", "id": name, "method": "tools/call", "params": { "name": name, "arguments": arguments } }),
        options,
    )
    .expect("response")
}

fn payload_of(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    serde_json::from_str(text).expect("payload json")
}

fn valid_search_args() -> Value {
    json!({ "pattern": "foo($$$ARGS)", "language": "typescript", "paths": ["src"] })
}

fn schema_of(name: &str) -> Value {
    ast_grep_mcp_tools()
        .into_iter()
        .find(|tool| tool.name == name)
        .map(|tool| tool.input_schema)
        .expect("descriptor")
}

fn property_of(name: &str, property: &str) -> Value {
    schema_of(name)["properties"][property].clone()
}

fn satisfies(schema: &Value, value: &Map<String, Value>) -> bool {
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let present = required
            .iter()
            .all(|key| key.as_str().is_some_and(|key| value.contains_key(key)));
        if !present {
            return false;
        }
    }
    if let Some(negated) = schema.get("not")
        && satisfies(negated, value)
    {
        return false;
    }
    if let Some(one_of) = schema.get("oneOf").and_then(Value::as_array) {
        let matched = one_of
            .iter()
            .filter(|branch| satisfies(branch, value))
            .count();
        if matched != 1 {
            return false;
        }
    }
    true
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("not an object: {other}"),
    }
}

struct ChannelReader {
    receiver: mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            match self.receiver.recv() {
                Ok(chunk) => self.pending = chunk,
                Err(mpsc::RecvError) => return Ok(0),
            }
        }
        let count = self.pending.len().min(buf.len());
        buf[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

fn run_to_end(input: &str, options: AstGrepMcpOptions) -> (ServerOutcome, String) {
    let mut output = Vec::new();
    let outcome = run_mcp_stdio_server(
        Cursor::new(input.to_owned().into_bytes()),
        &mut output,
        options,
    )
    .expect("server");
    (outcome, String::from_utf8(output).expect("utf8 output"))
}

#[test]
fn initialize_pins_server_info_and_capabilities() {
    let response = handle_default(json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": { "name": "todo-8", "version": "0.0.0" } },
    }));
    assert_eq!(
        response,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "ast_grep", "version": "0.1.0" },
                "protocolVersion": "2024-11-05",
            },
        })
    );
}

#[test]
fn initialize_without_protocol_version_falls_back() {
    let response = handle_default(
        json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": {} }),
    );
    assert_eq!(response["result"]["protocolVersion"], json!("2024-11-05"));
}

#[test]
fn initialize_echoes_newer_protocol_version() {
    let response = handle_default(json!({
        "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": { "protocolVersion": "2025-06-18" },
    }));
    assert_eq!(response["result"]["protocolVersion"], json!("2025-06-18"));
}

#[test]
fn tools_list_returns_exact_raw_names() {
    let response = handle_default(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools")
        .clone();
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, vec!["search", "rewrite", "scan"]);
    for tool in &tools {
        assert!(
            !tool["description"]
                .as_str()
                .expect("description")
                .is_empty()
        );
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert!(schema["required"].is_array());
    }
    let descriptor_names: Vec<String> = ast_grep_mcp_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect();
    assert_eq!(descriptor_names, vec!["search", "rewrite", "scan"]);
}

#[test]
fn ping_returns_empty_result() {
    let response = handle_default(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }));
    assert_eq!(response, json!({ "jsonrpc": "2.0", "id": 3, "result": {} }));
}

#[test]
fn notifications_initialized_produces_no_response() {
    let response = handle(
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        &stub_options(),
    );
    assert_eq!(response, None);
}

#[test]
fn non_object_request_is_invalid_request() {
    let expected = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "Invalid Request" } });
    assert_eq!(handle_default(json!("not-an-object")), expected);
    assert_eq!(handle_default(json!(["array"])), expected);
}

#[test]
fn unknown_method_is_method_not_found() {
    let response = handle_default(json!({ "jsonrpc": "2.0", "id": 9, "method": "resources/list" }));
    assert_eq!(response["error"]["code"], json!(-32601));
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("resources/list")
    );
}

#[test]
fn tools_call_without_name_is_invalid_params() {
    let missing_name = handle_default(
        json!({ "jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": { "arguments": {} } }),
    );
    assert_eq!(
        missing_name,
        json!({ "jsonrpc": "2.0", "id": 10, "error": { "code": -32602, "message": "tools/call requires params.name" } })
    );
    let missing_params =
        handle_default(json!({ "jsonrpc": "2.0", "id": 11, "method": "tools/call" }));
    assert_eq!(missing_params["error"]["code"], json!(-32602));
}

#[test]
fn malformed_stdio_line_writes_parse_error_envelope() {
    let (_, output) = run_to_end("garbage\n", stub_options());
    let first_line = output.lines().next().expect("one response line");
    let parsed: Value = serde_json::from_str(first_line).expect("json");
    assert_eq!(parsed["id"], Value::Null);
    assert_eq!(parsed["error"]["code"], json!(-32700));
}

#[test]
fn scan_schema_publishes_rule_source_one_of() {
    let schema = schema_of("scan");
    assert_eq!(
        schema["oneOf"],
        json!([
            { "type": "object", "required": ["ruleFile"], "not": { "required": ["inlineRules"] } },
            { "type": "object", "required": ["inlineRules"], "not": { "required": ["ruleFile"] } },
        ])
    );
    assert_eq!(schema["required"], json!(["paths"]));
}

#[test]
fn scan_args_without_rule_source_are_schema_invalid() {
    assert!(!satisfies(
        &schema_of("scan"),
        &object(json!({ "paths": ["src"] }))
    ));
}

#[test]
fn scan_args_with_both_rule_sources_are_schema_invalid() {
    let args = object(json!({ "paths": ["src"], "ruleFile": "r.yml", "inlineRules": "id: x" }));
    assert!(!satisfies(&schema_of("scan"), &args));
}

#[test]
fn scan_args_with_one_rule_source_are_schema_valid() {
    let schema = schema_of("scan");
    assert!(satisfies(
        &schema,
        &object(json!({ "paths": ["src"], "ruleFile": "r.yml" }))
    ));
    assert!(satisfies(
        &schema,
        &object(json!({ "paths": ["src"], "inlineRules": "id: x" }))
    ));
}

fn assert_byte_budget(property: &Value, budget: &str) {
    let description = property["description"].as_str().expect("description");
    assert!(description.contains(budget), "{description}");
    assert!(description.contains("BYTES"), "{description}");
    assert_eq!(property.get("maxLength"), None);
}

#[test]
fn search_pattern_publishes_16_kib_byte_budget() {
    assert_byte_budget(&property_of("search", "pattern"), "16 KiB");
}

#[test]
fn rewrite_publishes_pattern_and_rewrite_byte_budgets() {
    assert_byte_budget(&property_of("rewrite", "pattern"), "16 KiB");
    assert_byte_budget(&property_of("rewrite", "rewrite"), "64 KiB");
}

#[test]
fn scan_inline_rules_publishes_64_kib_byte_budget() {
    assert_byte_budget(&property_of("scan", "inlineRules"), "64 KiB");
}

#[test]
fn code_point_properties_keep_numeric_max_length() {
    assert_eq!(property_of("search", "selector")["maxLength"], json!(128));
    assert_eq!(property_of("search", "workdir")["maxLength"], json!(4096));
    assert_eq!(property_of("scan", "ruleFile")["maxLength"], json!(4096));
}

#[test]
fn stdio_initialize_answers_with_server_info_and_no_idle_timeout() {
    type LifecycleEvents = Arc<Mutex<Vec<(String, Option<McpLogFields>)>>>;
    let lifecycle: LifecycleEvents = Arc::default();
    let sink = Arc::clone(&lifecycle);
    let options = AstGrepMcpOptions {
        lifecycle_log: Some(Arc::new(
            move |event: &str, fields: Option<&McpLogFields>| {
                sink.lock()
                    .expect("lifecycle")
                    .push((event.to_owned(), fields.cloned()));
            },
        )),
        ..stub_options()
    };
    let (_, output) = run_to_end(
        "{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"initialize\"}\n",
        options,
    );
    assert!(
        output.contains("\"serverInfo\":{\"name\":\"ast_grep\""),
        "{output}"
    );
    let events = lifecycle.lock().expect("lifecycle").clone();
    let started = events
        .iter()
        .find(|(event, _)| event == "stdio_started")
        .and_then(|(_, fields)| fields.clone())
        .expect("stdio_started event");
    assert_eq!(started.get("idle_timeout_ms"), Some(&json!(0)));
}

#[test]
fn missing_sg_binary_still_answers_protocol() {
    let options = AstGrepMcpOptions {
        resolve_sg_path: Some(Arc::new(|| {
            Err(SgResolveError {
                message: "ast-grep binary not found".to_owned(),
                hints: Vec::new(),
            })
        })),
        ..AstGrepMcpOptions::default()
    };
    let (_, output) = run_to_end(
        "{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"tools/list\"}\n",
        options,
    );
    for name in ["search", "rewrite", "scan"] {
        assert!(output.contains(&format!("\"name\":\"{name}\"")), "{name}");
    }
}

#[test]
fn parent_exit_aborts_the_active_hung_call() {
    let parent_alive = Arc::new(AtomicBool::new(true));
    let probe_flag = Arc::clone(&parent_alive);
    let (started_tx, started_rx) = mpsc::channel::<AbortSignal>();
    let (aborted_tx, aborted_rx) = mpsc::channel::<String>();
    let started_tx = Mutex::new(started_tx);
    let aborted_tx = Mutex::new(aborted_tx);
    let search: ToolExecutor = Arc::new(move |_, _, signal| {
        let signal = signal.expect("signal").clone();
        started_tx
            .lock()
            .expect("started")
            .send(signal.clone())
            .expect("send started");
        let reason = signal
            .wait_for_abort(BOUND)
            .unwrap_or_else(|| "never aborted".to_owned());
        aborted_tx
            .lock()
            .expect("aborted")
            .send(reason)
            .expect("send aborted");
        Ok(
            json!({ "schemaVersion": 1, "ok": false, "kind": "search", "error": { "code": "ABORTED", "message": "aborted" } }),
        )
    });
    let options = AstGrepMcpOptions {
        parent_watchdog: Some(ParentWatchdogConfig {
            parent_pid: Some(4242),
            poll_interval_ms: Some(10),
            probe_alive: Some(Arc::new(move |_| probe_flag.load(Ordering::SeqCst))),
        }),
        ..with_executors(AstGrepToolExecutors {
            search: Some(search),
            ..AstGrepToolExecutors::default()
        })
    };
    let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let reader = ChannelReader {
            receiver: input_rx,
            pending: Vec::new(),
        };
        let outcome = run_mcp_stdio_server(reader, &mut output, options).expect("server");
        done_tx.send(outcome).expect("send outcome");
    });
    input_tx
        .send(b"{\"jsonrpc\":\"2.0\",\"id\":\"hang\",\"method\":\"tools/call\",\"params\":{\"name\":\"search\",\"arguments\":{\"pattern\":\"foo($$$ARGS)\",\"language\":\"typescript\",\"paths\":[\"src\"]}}}\n".to_vec())
        .expect("write request");
    let signal = started_rx.recv_timeout(BOUND).expect("tool call start");
    assert!(!signal.is_aborted());

    parent_alive.store(false, Ordering::SeqCst);

    let reason = aborted_rx.recv_timeout(BOUND).expect("active call abort");
    assert!(reason.contains("parent process exited"), "{reason}");
    assert!(signal.is_aborted());
    assert_eq!(
        done_rx.recv_timeout(BOUND).expect("server termination"),
        ServerOutcome::ParentExit
    );
    drop(input_tx);
}

#[test]
fn later_call_receives_fresh_unaborted_signal() {
    let signals: Arc<Mutex<Vec<AbortSignal>>> = Arc::default();
    let sink = Arc::clone(&signals);
    let search: ToolExecutor = Arc::new(move |_, _, signal| {
        sink.lock()
            .expect("signals")
            .push(signal.expect("signal").clone());
        Ok(json!({ "schemaVersion": 1, "ok": true, "kind": "search" }))
    });
    let options = with_executors(AstGrepToolExecutors {
        search: Some(search),
        ..AstGrepToolExecutors::default()
    });
    let line = |id: u32| {
        format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"search\",\"arguments\":{{\"pattern\":\"foo($$$ARGS)\",\"language\":\"typescript\",\"paths\":[\"src\"]}}}}}}\n"
        )
    };
    run_to_end(&format!("{}{}", line(1), line(2)), options);
    let signals = signals.lock().expect("signals").clone();
    assert_eq!(signals.len(), 2);
    assert!(signals.iter().all(|signal| !signal.is_aborted()));
    signals[0].abort("probe identity");
    assert!(
        !signals[1].is_aborted(),
        "each call must get its own signal"
    );
}

#[test]
fn known_tools_dispatch_to_matching_executor_with_sg_path() {
    let seen: Arc<Mutex<Vec<(String, Value, String)>>> = Arc::default();
    let recorder = |tool: &'static str| -> ToolExecutor {
        let seen = Arc::clone(&seen);
        Arc::new(move |input, sg_path, _| {
            seen.lock()
                .expect("seen")
                .push((tool.to_owned(), input.clone(), sg_path.to_owned()));
            Ok(json!({ "schemaVersion": 1, "ok": true, "kind": tool }))
        })
    };
    let options = with_executors(AstGrepToolExecutors {
        search: Some(recorder("search")),
        rewrite: Some(recorder("rewrite")),
        scan: Some(recorder("scan")),
    });
    for (name, args) in [
        ("search", valid_search_args()),
        ("rewrite", json!({ "pattern": "$A" })),
        ("scan", json!({ "pattern": "$A" })),
    ] {
        let response = call(name, args, &options);
        assert_eq!(response["result"]["isError"], json!(false), "{name}");
        assert_eq!(payload_of(&response)["kind"], json!(name));
    }
    let seen = seen.lock().expect("seen").clone();
    let tools: Vec<&str> = seen.iter().map(|(tool, _, _)| tool.as_str()).collect();
    assert_eq!(tools, vec!["search", "rewrite", "scan"]);
    assert!(seen.iter().all(|(_, _, sg_path)| sg_path == SG_PATH));
    assert_eq!(seen[1].1, json!({ "pattern": "$A" }));
    assert_eq!(seen[2].1, json!({ "pattern": "$A" }));
    let search_input = &seen[0].1;
    for (key, expected) in [
        ("pattern", json!("foo($$$ARGS)")),
        ("language", json!("typescript")),
        ("paths", json!(["src"])),
        ("strictness", json!("smart")),
        ("maxMatches", json!(50)),
        ("timeoutMs", json!(300_000)),
    ] {
        assert_eq!(search_input[key], expected, "{key}");
    }
}

#[test]
fn omitted_arguments_reach_self_parsing_tool_as_empty_object() {
    let received: Arc<Mutex<Option<Value>>> = Arc::default();
    let sink = Arc::clone(&received);
    let scan: ToolExecutor = Arc::new(move |input, _, _| {
        *sink.lock().expect("received") = Some(input.clone());
        Ok(json!({ "schemaVersion": 1, "ok": true, "kind": "scan" }))
    });
    let response = handle(
        json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "scan" } }),
        &with_executors(AstGrepToolExecutors {
            scan: Some(scan),
            ..AstGrepToolExecutors::default()
        }),
    )
    .expect("response");
    assert_eq!(*received.lock().expect("received"), Some(json!({})));
    assert_eq!(response["result"]["isError"], json!(false));
}

fn never_search(executed: &Arc<AtomicBool>) -> AstGrepMcpOptions {
    let flag = Arc::clone(executed);
    let search: ToolExecutor = Arc::new(move |_, _, _| {
        flag.store(true, Ordering::SeqCst);
        Ok(json!({ "schemaVersion": 1, "ok": true, "kind": "search" }))
    });
    with_executors(AstGrepToolExecutors {
        search: Some(search),
        ..AstGrepToolExecutors::default()
    })
}

#[test]
fn empty_search_arguments_are_invalid_argument_preflight() {
    let executed = Arc::new(AtomicBool::new(false));
    let response = call("search", json!({}), &never_search(&executed));
    assert!(!executed.load(Ordering::SeqCst));
    assert_eq!(response.get("error"), None);
    assert_eq!(response["result"]["isError"], json!(true));
    let payload = payload_of(&response);
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["code"], json!("INVALID_ARGUMENT"));
    assert_eq!(payload["error"]["phase"], json!("preflight"));
    let message = payload["error"]["message"].as_str().expect("message");
    assert!(message.contains("pattern"), "{message}");
    assert!(!message.contains("Cannot read properties"), "{message}");
}

#[test]
fn unknown_search_property_is_rejected() {
    let executed = Arc::new(AtomicBool::new(false));
    let mut args = valid_search_args();
    args["unknown"] = json!(true);
    let response = call("search", args, &never_search(&executed));
    assert!(!executed.load(Ordering::SeqCst));
    assert_eq!(response["result"]["isError"], json!(true));
    let payload = payload_of(&response);
    assert_eq!(payload["error"]["code"], json!("INVALID_ARGUMENT"));
    assert!(
        payload["error"]["message"]
            .as_str()
            .expect("message")
            .contains("unknown")
    );
    assert_eq!(payload["error"]["language"], json!("typescript"));
}

#[test]
fn oversize_search_pattern_is_invalid_argument() {
    let mut args = valid_search_args();
    args["pattern"] = json!("a".repeat(16 * 1024 + 1));
    let response = call("search", args, &stub_options());
    assert_eq!(response["result"]["isError"], json!(true));
    let payload = payload_of(&response);
    assert_eq!(payload["error"]["code"], json!("INVALID_ARGUMENT"));
    assert!(
        payload["error"]["message"]
            .as_str()
            .expect("message")
            .contains("16384 bytes")
    );
}

#[test]
fn unknown_tool_name_is_domain_invalid_argument() {
    let response = call("mcp__ast_grep__search", json!({}), &stub_options());
    assert_eq!(response.get("error"), None);
    assert_eq!(response["result"]["isError"], json!(true));
    let payload = payload_of(&response);
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["code"], json!("INVALID_ARGUMENT"));
    assert!(
        payload["error"]["message"]
            .as_str()
            .expect("message")
            .contains("mcp__ast_grep__search")
    );
}

#[test]
fn ok_false_payload_sets_is_error_and_keeps_code() {
    let scan: ToolExecutor = Arc::new(|_, _, _| {
        Ok(json!({
            "schemaVersion": 1, "ok": false, "kind": "scan",
            "error": { "code": "RULE_PARSE_FAILED", "message": "bad yaml", "retryable": false, "phase": "preview", "details": {} },
            "durationMs": 3,
        }))
    });
    let response = call(
        "scan",
        json!({}),
        &with_executors(AstGrepToolExecutors {
            scan: Some(scan),
            ..AstGrepToolExecutors::default()
        }),
    );
    assert_eq!(response["result"]["isError"], json!(true));
    assert_eq!(
        payload_of(&response)["error"]["code"],
        json!("RULE_PARSE_FAILED")
    );
}

#[test]
fn executor_failure_becomes_sg_failed() {
    let search: ToolExecutor = Arc::new(|_, _, _| Err("stub blew up".to_owned()));
    let response = call(
        "search",
        valid_search_args(),
        &with_executors(AstGrepToolExecutors {
            search: Some(search),
            ..AstGrepToolExecutors::default()
        }),
    );
    assert_eq!(response.get("error"), None);
    assert_eq!(response["result"]["isError"], json!(true));
    let payload = payload_of(&response);
    assert_eq!(payload["ok"], json!(false));
    assert_eq!(payload["error"]["code"], json!("SG_FAILED"));
    assert!(
        payload["error"]["message"]
            .as_str()
            .expect("message")
            .contains("stub blew up")
    );
}

#[test]
fn unresolvable_sg_binary_is_binary_not_found_with_hints() {
    let options = AstGrepMcpOptions {
        resolve_sg_path: Some(Arc::new(|| {
            Err(SgResolveError {
                message: "ast-grep binary not found".to_owned(),
                hints: vec!["brew install ast-grep".to_owned()],
            })
        })),
        ..AstGrepMcpOptions::default()
    };
    let response = call("search", json!({}), &options);
    assert_eq!(response["result"]["isError"], json!(true));
    let payload = payload_of(&response);
    assert_eq!(payload["error"]["code"], json!("BINARY_NOT_FOUND"));
    assert_eq!(
        payload["error"]["details"]["hints"],
        json!(["brew install ast-grep"])
    );
}

#[test]
fn taxonomy_holds_19_documented_codes() {
    assert!(AST_GREP_ERROR_CODES.contains(&"RULE_PARSE_FAILED"));
    assert!(AST_GREP_ERROR_CODES.contains(&"BINARY_NOT_FOUND"));
    assert_eq!(
        AST_GREP_ERROR_CODES.iter().collect::<BTreeSet<_>>().len(),
        19
    );
}

#[test]
fn smoke_initialize_through_package_entry() {
    let response = handle_default(json!({
        "jsonrpc": "2.0", "id": "smoke", "method": "initialize", "params": { "protocolVersion": "2025-06-18" },
    }));
    assert_eq!(
        response["result"]["serverInfo"]["name"],
        json!(AST_GREP_MCP_NAME)
    );
    assert_eq!(response["result"]["protocolVersion"], json!("2025-06-18"));
}
