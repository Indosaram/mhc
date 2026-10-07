//! Port of the pinned `git-bash-mcp` MCP tests: `mcp.test.ts`, `mcp-startup.test.ts`,
//! `mcp-timeout.test.ts`, `mcp-schema.test.ts` and `mcp-protocol-pin.test.ts`.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::sync::Mutex;

use git_bash_mcp::EXEC_COMMAND_TIMEOUT_ENV_KEYS;
use git_bash_mcp::GIT_BASH_ENV_KEY;
use git_bash_mcp::GitBashMcpOptions;
use git_bash_mcp::GitBashRunInput;
use git_bash_mcp::GitBashRunResult;
use git_bash_mcp::GitBashServerOutcome;
use git_bash_mcp::RunGitBashCommand;
use git_bash_mcp::handle_git_bash_mcp_request;
use git_bash_mcp::run_mcp_stdio_server;
use mcp_stdio_core::McpLifecycleLog;
use mcp_stdio_core::McpLogFields;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

const PROGRAM_FILES_GIT_BASH: &str = "C:\\Program Files\\Git\\bin\\bash.exe";
const PROGRAM_FILES_X86_GIT_BASH: &str = "C:\\Program Files (x86)\\Git\\bin\\bash.exe";
const SYSTEM32_BASH: &str = "C:\\Windows\\System32\\bash.exe";
const PATH_GIT_BASH: &str = "D:\\Git\\bin\\bash.exe";
const ENV_BASH: &str = "C:\\Tools\\Git\\bin\\bash.exe";
const ENV_BASH_ONLY: &[&str] = &[ENV_BASH];
const PROGRAM_FILES_ONLY: &[&str] = &[PROGRAM_FILES_GIT_BASH];
const SYSTEM32_AND_PATH: &[&str] = &[SYSTEM32_BASH, PATH_GIT_BASH];
const NOTHING_EXISTS: &[&str] = &[];

fn exists_only(paths: &'static [&'static str]) -> Arc<dyn Fn(&str) -> bool + Send + Sync> {
    Arc::new(move |path: &str| paths.contains(&path))
}

fn no_paths() -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
    Arc::new(|| Vec::new())
}

fn listed(paths: &'static [&'static str]) -> Arc<dyn Fn() -> Vec<String> + Send + Sync> {
    Arc::new(move || paths.iter().map(|path| (*path).to_owned()).collect())
}

/// Mirrors the pinned `windowsOptions()`: an env override that exists on disk.
fn windows_options() -> GitBashMcpOptions {
    GitBashMcpOptions {
        platform: Some("win32".to_owned()),
        env: Some(HashMap::from([(
            GIT_BASH_ENV_KEY.to_owned(),
            PROGRAM_FILES_GIT_BASH.to_owned(),
        )])),
        exists: Some(exists_only(PROGRAM_FILES_ONLY)),
        where_bash: Some(no_paths()),
        ..GitBashMcpOptions::default()
    }
}

fn non_windows_options(platform: &str) -> GitBashMcpOptions {
    GitBashMcpOptions {
        platform: Some(platform.to_owned()),
        env: Some(HashMap::new()),
        exists: Some(exists_only(NOTHING_EXISTS)),
        where_bash: Some(no_paths()),
        ..GitBashMcpOptions::default()
    }
}

fn handle(request: Value, options: &GitBashMcpOptions) -> Value {
    handle_optional(request, options).expect("response")
}

fn handle_optional(request: Value, options: &GitBashMcpOptions) -> Option<Value> {
    handle_git_bash_mcp_request(&request, options)
        .map(|response| serde_json::to_value(response).expect("serialize response"))
}

fn call(name: &str, arguments: Value, options: &GitBashMcpOptions) -> Value {
    handle(
        json!({
            "jsonrpc": "2.0",
            "id": name,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }),
        options,
    )
}

fn text_of(response: &Value) -> String {
    response["result"]["content"][0]["text"]
        .as_str()
        .expect("content text")
        .to_owned()
}

fn payload_of(response: &Value) -> Value {
    serde_json::from_str(&text_of(response)).expect("payload json")
}

fn is_error(response: &Value) -> bool {
    response["result"]["isError"].as_bool().expect("isError")
}

fn tool_names(response: &Value) -> Vec<String> {
    response["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CapturedRun {
    bash_path: Option<String>,
    command: Option<String>,
    cwd: Option<String>,
    timeout_ms: Option<u64>,
}

fn capturing_runner(sink: Arc<Mutex<CapturedRun>>) -> Arc<RunGitBashCommand> {
    Arc::new(move |input: &GitBashRunInput| {
        let mut captured = sink.lock().expect("capture");
        captured.bash_path = Some(input.bash_path.clone());
        captured.command = Some(input.command.clone());
        captured.cwd = input.cwd.clone();
        captured.timeout_ms = Some(input.timeout_ms);
        Ok(GitBashRunResult {
            exit_code: Some(0),
            stdout: "ok\n".to_owned(),
            stderr: String::new(),
            timed_out: false,
        })
    })
}

fn run_stdio(input: &str, options: GitBashMcpOptions) -> (GitBashServerOutcome, String) {
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
fn timeout_env_keys_preserve_upstream_strings() {
    assert_eq!(GIT_BASH_ENV_KEY, "OMO_CODEX_GIT_BASH_PATH");
    assert_eq!(
        EXEC_COMMAND_TIMEOUT_ENV_KEYS,
        [
            "OMO_CODEX_GIT_BASH_TIMEOUT_MS",
            "OMO_CODEX_EXEC_COMMAND_TIMEOUT_MS",
            "CODEX_EXEC_COMMAND_TIMEOUT_MS",
            "EXEC_COMMAND_TIMEOUT_MS",
        ]
    );
}

#[test]
fn which_bash_returns_env_path_and_source() {
    let options = GitBashMcpOptions {
        platform: Some("win32".to_owned()),
        env: Some(HashMap::from([(
            "OMO_CODEX_GIT_BASH_PATH".to_owned(),
            ENV_BASH.to_owned(),
        )])),
        exists: Some(exists_only(ENV_BASH_ONLY)),
        where_bash: Some(no_paths()),
        ..GitBashMcpOptions::default()
    };
    let response = call("which_bash", json!({}), &options);

    let payload = payload_of(&response);
    assert!(!is_error(&response));
    assert_eq!(payload["found"], json!(true));
    assert_eq!(payload["source"], json!("env"));
    assert_eq!(payload["path"], json!(ENV_BASH));
    assert_eq!(payload["checkedPaths"], json!([ENV_BASH]));
}

#[test]
fn diagnose_lists_every_probed_path() {
    let options = GitBashMcpOptions {
        platform: Some("win32".to_owned()),
        env: Some(HashMap::new()),
        exists: Some(exists_only(SYSTEM32_AND_PATH)),
        where_bash: Some(listed(SYSTEM32_AND_PATH)),
        ..GitBashMcpOptions::default()
    };
    let response = call("diagnose", json!({}), &options);

    let payload = payload_of(&response);
    assert!(!is_error(&response));
    assert_eq!(payload["enabled"], json!(true));
    assert_eq!(payload["status"], json!("ready"));
    assert_eq!(
        payload["resolution"]["checkedPaths"],
        json!([
            PROGRAM_FILES_GIT_BASH,
            PROGRAM_FILES_X86_GIT_BASH,
            SYSTEM32_BASH,
            PATH_GIT_BASH,
        ])
    );
}

#[test]
fn diagnose_reports_disabled_off_windows() {
    let response = call("diagnose", json!({}), &non_windows_options("darwin"));

    let payload = payload_of(&response);
    assert!(!is_error(&response));
    assert_eq!(payload["enabled"], json!(false));
    assert_eq!(payload["platform"], json!("darwin"));
    assert_eq!(
        payload["status"],
        json!("disabled: git_bash command execution is only exposed on native Windows")
    );
}

#[test]
fn tools_list_hides_run_off_windows() {
    let response = handle(
        json!({ "jsonrpc": "2.0", "id": "tools", "method": "tools/list" }),
        &non_windows_options("linux"),
    );

    assert_eq!(tool_names(&response), vec!["which_bash", "diagnose"]);
}

#[test]
fn run_uses_resolved_git_bash_and_command_payload() {
    let sink = Arc::new(Mutex::new(CapturedRun::default()));
    let options = GitBashMcpOptions {
        run_git_bash: Some(capturing_runner(Arc::clone(&sink))),
        ..windows_options()
    };
    let response = call(
        "run",
        json!({ "command": "printf ok", "cwd": "C:\\repo", "timeout_ms": 5000 }),
        &options,
    );

    let payload = payload_of(&response);
    assert!(!is_error(&response));
    assert_eq!(payload["stdout"], json!("ok\n"));
    assert_eq!(payload["exitCode"], json!(0));
    assert_eq!(payload["timedOut"], json!(false));
    assert_eq!(
        *sink.lock().expect("capture"),
        CapturedRun {
            bash_path: Some(PROGRAM_FILES_GIT_BASH.to_owned()),
            command: Some("printf ok".to_owned()),
            cwd: Some("C:\\repo".to_owned()),
            timeout_ms: Some(5_000),
        }
    );
}

#[test]
fn run_rejects_blank_command_without_spawning() {
    let sink = Arc::new(Mutex::new(CapturedRun::default()));
    let options = GitBashMcpOptions {
        run_git_bash: Some(capturing_runner(Arc::clone(&sink))),
        ..windows_options()
    };
    let response = call("run", json!({ "command": "   " }), &options);

    assert!(is_error(&response));
    assert_eq!(text_of(&response), "run.command must be a non-empty string.");
    assert_eq!(*sink.lock().expect("capture"), CapturedRun::default());
}

#[test]
fn stdio_server_stays_disabled_off_windows() {
    let (outcome, output) = run_stdio(
        "{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"initialize\"}\n",
        non_windows_options("darwin"),
    );

    assert_eq!(outcome, GitBashServerOutcome::Disabled);
    assert_eq!(output, "");
}

#[test]
fn stdio_server_stays_disabled_on_windows_without_git_bash() {
    let options = GitBashMcpOptions {
        platform: Some("win32".to_owned()),
        env: Some(HashMap::new()),
        exists: Some(exists_only(NOTHING_EXISTS)),
        where_bash: Some(no_paths()),
        ..GitBashMcpOptions::default()
    };
    let (outcome, output) = run_stdio(
        "{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"initialize\"}\n",
        options,
    );

    assert_eq!(outcome, GitBashServerOutcome::Disabled);
    assert_eq!(output, "");
}

#[test]
fn stdio_server_serves_on_windows_with_git_bash_and_disables_idle_timeout() {
    let lifecycle: Arc<Mutex<Vec<(String, Option<McpLogFields>)>>> = Arc::default();
    let sink = Arc::clone(&lifecycle);
    let log: Arc<dyn McpLifecycleLog + Send + Sync> =
        Arc::new(move |event: &str, fields: Option<&McpLogFields>| {
            sink.lock()
                .expect("lifecycle")
                .push((event.to_owned(), fields.cloned()));
        });
    let options = GitBashMcpOptions {
        lifecycle_log: Some(log),
        ..windows_options()
    };

    let (outcome, output) = run_stdio(
        "{\"jsonrpc\":\"2.0\",\"id\":\"init\",\"method\":\"initialize\"}\n",
        options,
    );

    assert!(matches!(outcome, GitBashServerOutcome::Served(_)));
    assert!(output.contains("\"serverInfo\":{\"name\":\"git_bash\""), "{output}");
    let events = lifecycle.lock().expect("lifecycle").clone();
    let started = events
        .iter()
        .find(|(event, _)| event == "stdio_started")
        .and_then(|(_, fields)| fields.clone())
        .expect("stdio_started event");
    assert_eq!(started.get("idle_timeout_ms"), Some(&json!(0)));
}

#[test]
fn run_uses_inherited_default_timeout_with_workdir() {
    let sink = Arc::new(Mutex::new(CapturedRun::default()));
    let options = GitBashMcpOptions {
        run_git_bash: Some(capturing_runner(Arc::clone(&sink))),
        default_timeout_ms: Some(45_000.0),
        ..windows_options()
    };
    let response = call(
        "run",
        json!({ "command": "printf ok", "workdir": "C:\\repo" }),
        &options,
    );

    assert!(!is_error(&response));
    assert_eq!(
        *sink.lock().expect("capture"),
        CapturedRun {
            bash_path: Some(PROGRAM_FILES_GIT_BASH.to_owned()),
            command: Some("printf ok".to_owned()),
            cwd: Some("C:\\repo".to_owned()),
            timeout_ms: Some(45_000),
        }
    );
}

#[test]
fn run_per_call_timeout_wins_over_inherited() {
    let sink = Arc::new(Mutex::new(CapturedRun::default()));
    let options = GitBashMcpOptions {
        run_git_bash: Some(capturing_runner(Arc::clone(&sink))),
        default_timeout_ms: Some(45_000.0),
        ..windows_options()
    };
    let response = call(
        "run",
        json!({ "command": "printf ok", "timeout": 7000 }),
        &options,
    );

    assert!(!is_error(&response));
    assert_eq!(sink.lock().expect("capture").timeout_ms, Some(7_000));
}

#[test]
fn run_uses_exec_command_timeout_env_default() {
    let sink = Arc::new(Mutex::new(CapturedRun::default()));
    let options = GitBashMcpOptions {
        run_git_bash: Some(capturing_runner(Arc::clone(&sink))),
        env: Some(HashMap::from([
            (
                GIT_BASH_ENV_KEY.to_owned(),
                PROGRAM_FILES_GIT_BASH.to_owned(),
            ),
            (
                "OMO_CODEX_EXEC_COMMAND_TIMEOUT_MS".to_owned(),
                "65000".to_owned(),
            ),
        ])),
        ..windows_options()
    };
    let response = call("run", json!({ "command": "printf ok" }), &options);

    assert!(!is_error(&response));
    assert_eq!(sink.lock().expect("capture").timeout_ms, Some(65_000));
}

#[test]
fn run_schema_matches_shell_command_conventions() {
    let response = handle(
        json!({ "jsonrpc": "2.0", "id": "tools", "method": "tools/list" }),
        &windows_options(),
    );

    let tools = response["result"]["tools"].as_array().expect("tools").clone();
    let run = tools
        .iter()
        .find(|tool| tool["name"] == json!("run"))
        .expect("run tool");
    let description = run["description"].as_str().expect("description");
    assert!(description.contains("exec_command"), "{description}");
    let schema = &run["inputSchema"];
    let properties = schema["properties"].as_object().expect("properties");
    assert_eq!(
        properties.keys().map(String::as_str).collect::<Vec<_>>(),
        vec!["command", "timeout", "workdir", "description"]
    );
    assert_eq!(properties["timeout"]["type"], json!("integer"));
    assert_eq!(properties["timeout"]["minimum"], json!(1));
    assert_eq!(properties["timeout"]["maximum"], json!(1_800_000));
    assert!(
        properties["workdir"]["description"]
            .as_str()
            .expect("workdir description")
            .contains("Use this instead of")
    );
    assert!(
        properties["description"]["description"]
            .as_str()
            .expect("description description")
            .contains("5-10 words")
    );
    assert_eq!(schema["required"], json!(["command"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(
        tool_names(&response),
        vec!["run", "which_bash", "diagnose"]
    );
}

#[test]
fn tools_list_hides_run_on_windows_without_git_bash() {
    let options = GitBashMcpOptions {
        platform: Some("win32".to_owned()),
        env: Some(HashMap::new()),
        exists: Some(exists_only(NOTHING_EXISTS)),
        where_bash: Some(no_paths()),
        ..GitBashMcpOptions::default()
    };
    let response = handle(
        json!({ "jsonrpc": "2.0", "id": "tools", "method": "tools/list" }),
        &options,
    );

    assert_eq!(tool_names(&response), vec!["which_bash", "diagnose"]);
}

#[test]
fn initialize_pins_server_info_and_capabilities() {
    let response = handle(
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "todo-1003", "version": "0.0.0" },
            },
        }),
        &windows_options(),
    );

    assert_eq!(
        response,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "git_bash", "version": "0.1.0" },
                "protocolVersion": "2024-11-05",
            },
        })
    );
}

#[test]
fn initialize_without_protocol_version_falls_back() {
    let response = handle(
        json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize" }),
        &windows_options(),
    );

    assert_eq!(response["result"]["protocolVersion"], json!("2024-11-05"));
}

#[test]
fn malformed_stdio_line_writes_method_not_found_envelope() {
    let (_, output) = run_stdio("garbage\n", windows_options());

    let first_line = output.lines().next().expect("one response line");
    let parsed: Value = serde_json::from_str(first_line).expect("json");
    assert_eq!(
        parsed,
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32601, "message": "Method not found" },
        })
    );
}

#[test]
fn run_off_windows_returns_native_windows_error() {
    let response = call("run", json!({ "command": "printf ok" }), &non_windows_options("linux"));

    assert!(is_error(&response));
    assert_eq!(
        text_of(&response),
        "git_bash run is only available on native Windows."
    );
}

#[test]
fn run_on_windows_without_git_bash_returns_install_hint() {
    let options = GitBashMcpOptions {
        platform: Some("win32".to_owned()),
        env: Some(HashMap::new()),
        exists: Some(exists_only(NOTHING_EXISTS)),
        where_bash: Some(no_paths()),
        ..GitBashMcpOptions::default()
    };
    let response = call("run", json!({ "command": "printf ok" }), &options);

    assert!(is_error(&response));
    let payload = payload_of(&response);
    assert_eq!(payload["found"], json!(false));
    let hint = payload["installHint"].as_str().expect("installHint");
    assert!(
        hint.contains("winget install --id Git.Git -e --source winget"),
        "{hint}"
    );
    assert!(
        hint.contains("OMO_CODEX_GIT_BASH_PATH=C:\\path\\to\\bash.exe"),
        "{hint}"
    );
}

#[test]
fn run_rejects_invalid_timeout_before_spawning() {
    let sink = Arc::new(Mutex::new(CapturedRun::default()));
    let options = GitBashMcpOptions {
        run_git_bash: Some(capturing_runner(Arc::clone(&sink))),
        ..windows_options()
    };
    let response = call(
        "run",
        json!({ "command": "printf ok", "timeout": 0 }),
        &options,
    );

    assert!(is_error(&response));
    assert_eq!(
        text_of(&response),
        "run.timeout must be an integer between 1 and 1800000."
    );
    assert_eq!(*sink.lock().expect("capture"), CapturedRun::default());
}

#[test]
fn run_rejects_blank_workdir_before_spawning() {
    let sink = Arc::new(Mutex::new(CapturedRun::default()));
    let options = GitBashMcpOptions {
        run_git_bash: Some(capturing_runner(Arc::clone(&sink))),
        ..windows_options()
    };
    let response = call(
        "run",
        json!({ "command": "printf ok", "workdir": "  " }),
        &options,
    );

    assert!(is_error(&response));
    assert_eq!(
        text_of(&response),
        "run.workdir must be a non-empty string when provided."
    );
    assert_eq!(*sink.lock().expect("capture"), CapturedRun::default());
}

#[test]
fn unknown_method_is_method_not_found() {
    let response = handle(
        json!({ "jsonrpc": "2.0", "id": 9, "method": "resources/list" }),
        &windows_options(),
    );

    assert_eq!(
        response,
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "error": { "code": -32601, "message": "Method not found" },
        })
    );
}

#[test]
fn notifications_initialized_produces_no_response() {
    let response = handle_optional(
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        &windows_options(),
    );

    assert_eq!(response, None);
}

#[test]
fn non_object_request_is_invalid_request() {
    let expected = json!({
        "jsonrpc": "2.0",
        "id": null,
        "error": { "code": -32600, "message": "Invalid Request" },
    });
    assert_eq!(handle(json!("not-an-object"), &windows_options()), expected);
    assert_eq!(handle(json!(["array"]), &windows_options()), expected);
}

#[test]
fn unknown_tool_reports_its_name() {
    let response = call("nope", json!({}), &windows_options());

    assert!(is_error(&response));
    assert_eq!(text_of(&response), "Unknown git_bash tool: nope");
}

#[test]
fn run_reports_a_runner_failure_as_error_text() {
    let options = GitBashMcpOptions {
        run_git_bash: Some(Arc::new(|_input: &GitBashRunInput| {
            Err("spawn failed".to_owned())
        })),
        ..windows_options()
    };
    let response = call("run", json!({ "command": "printf ok" }), &options);

    assert!(is_error(&response));
    assert_eq!(text_of(&response), "spawn failed");
}

#[test]
fn run_accepts_timeout_ms_alias_and_rejects_a_fractional_timeout() {
    let sink = Arc::new(Mutex::new(CapturedRun::default()));
    let options = GitBashMcpOptions {
        run_git_bash: Some(capturing_runner(Arc::clone(&sink))),
        ..windows_options()
    };

    let accepted = call(
        "run",
        json!({ "command": "printf ok", "timeout_ms": "2500" }),
        &options,
    );
    assert!(!is_error(&accepted));
    assert_eq!(sink.lock().expect("capture").timeout_ms, Some(2_500));

    let rejected = call(
        "run",
        json!({ "command": "printf ok", "timeout": 1.5 }),
        &options,
    );
    assert!(is_error(&rejected));
    assert_eq!(
        text_of(&rejected),
        "run.timeout must be an integer between 1 and 1800000."
    );
}

#[test]
fn stdio_server_writes_responses_for_a_windows_handshake() {
    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
    );
    let (outcome, output) = run_stdio(input, windows_options());

    assert!(matches!(outcome, GitBashServerOutcome::Served(_)));
    let responses: Vec<Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], json!("git_bash"));
    assert_eq!(
        tool_names(&responses[1]),
        vec!["run", "which_bash", "diagnose"]
    );
}

#[test]
fn stdio_server_settles_on_input_close() {
    let (outcome, _) = run_stdio("", windows_options());

    assert_eq!(
        outcome,
        GitBashServerOutcome::Served(mcp_stdio_core::ServerOutcome::InputClosed)
    );
}
