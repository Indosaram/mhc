//! Acceptance tests 2, 4, 5, 6, 7, 8 of `docs/cleanroom/specs/git-bash-mcp.md` §8, run against an
//! injected host so they pass on any platform.

mod common;

use common::{FakeHost, GIT_BASH, parse, server, tool_names};
use maho_git_bash::lookup::{PATH_ENV, PROGRAM_FILES_X86_BASH, WINGET_INSTALL};
use maho_git_bash::settings::{FALLBACK_TIMEOUT_MS, default_timeout_ms};
use maho_git_bash::{RunOutput, Server, ServerOptions, resolve};
use serde_json::{Value, json};
use std::collections::BTreeMap;

// --- Test 2: non-Windows -------------------------------------------------------------------

#[tokio::test]
async fn t2_non_windows_diagnose_reports_disabled_and_lists_two_tools() {
    let server = server(FakeHost::new("linux"));
    assert!(!server.is_available());
    let reply = server.call_tool("diagnose", None).await;
    assert!(!reply.is_error);
    let report = parse(&reply.text);
    assert_eq!(report["platform"], "linux");
    assert_eq!(report["enabled"], false);
    let status = report["status"].as_str().expect("status");
    assert_eq!(status.split_whitespace().next(), Some("disabled"));
    assert_eq!(report["resolution"]["found"], true);
    assert_eq!(report["resolution"]["source"], "not-required");
    assert_eq!(report["resolution"]["checkedPaths"], json!([]));
    assert!(report["resolution"].get("path").is_none());
    assert_eq!(tool_names(&server), ["which_bash", "diagnose"]);
}

#[tokio::test]
async fn t2_non_windows_run_is_windows_only() {
    let server = server(FakeHost::new("darwin"));
    let reply = server.call_tool("run", Some(&json!({"command": "ls"}))).await;
    assert!(reply.is_error);
    assert!(reply.text.contains("Windows-only"), "{}", reply.text);
    assert!(server.host().recorded().is_empty());
}

// --- Test 4: lookup ------------------------------------------------------------------------

#[test]
fn t4_env_var_valid_gives_source_env() {
    let path = r"D:\tools\Git\usr\bin\BASH.EXE";
    let host = FakeHost::new("win32")
        .with_env(PATH_ENV, &format!("  {path}  "))
        .with_file(path)
        .with_file(GIT_BASH);
    let value = resolve(&host).to_json();
    assert_eq!(value["found"], true);
    assert_eq!(value["source"], "env");
    assert_eq!(value["path"], path);
    assert_eq!(value["checkedPaths"], json!([path]));
}

#[test]
fn t4_env_var_missing_file_does_not_fall_back() {
    let path = r"D:\nowhere\bash.exe";
    let host = FakeHost::new("win32")
        .with_env(PATH_ENV, path)
        .with_file(GIT_BASH);
    let value = resolve(&host).to_json();
    assert_eq!(value["found"], false);
    assert_eq!(value["checkedPaths"], json!([path]));
}

#[test]
fn t4_env_var_not_bash_exe_does_not_fall_back() {
    let path = r"C:\Program Files\Git\bin\sh.exe";
    let host = FakeHost::new("win32")
        .with_env(PATH_ENV, path)
        .with_file(path)
        .with_file(GIT_BASH);
    let value = resolve(&host).to_json();
    assert_eq!(value["found"], false);
    assert_eq!(value["checkedPaths"], json!([path]));
}

#[test]
fn t4_blank_env_var_is_ignored() {
    let host = FakeHost::new("win32")
        .with_env(PATH_ENV, "   ")
        .with_file(GIT_BASH);
    let value = resolve(&host).to_json();
    assert_eq!(value["source"], "program-files");
}

#[test]
fn t4_neutral_alias_is_checked_first() {
    let alias = r"E:\git\bin\bash.exe";
    let host = FakeHost::new("win32")
        .with_env("MHC_GIT_BASH_PATH", alias)
        .with_env(PATH_ENV, GIT_BASH)
        .with_file(alias)
        .with_file(GIT_BASH);
    let value = resolve(&host).to_json();
    assert_eq!(value["path"], alias);
    assert_eq!(value["source"], "env");
}

#[test]
fn t4_program_files_before_x86() {
    let both = FakeHost::new("win32")
        .with_file(GIT_BASH)
        .with_file(PROGRAM_FILES_X86_BASH);
    let value = resolve(&both).to_json();
    assert_eq!(value["source"], "program-files");
    assert_eq!(value["path"], GIT_BASH);
    assert_eq!(value["checkedPaths"], json!([GIT_BASH]));

    let x86 = FakeHost::new("win32").with_file(PROGRAM_FILES_X86_BASH);
    let value = resolve(&x86).to_json();
    assert_eq!(value["source"], "program-files-x86");
    assert_eq!(value["checkedPaths"], json!([GIT_BASH, PROGRAM_FILES_X86_BASH]));
}

#[test]
fn t4_where_output_skips_wsl_launchers() {
    let system32 = r"C:\Windows\System32\bash.exe";
    let windows_apps = r"C:\Users\me\AppData\Local/Microsoft/WindowsApps\bash.exe";
    let git = r"D:\PortableGit\bin\bash.exe";
    let host = FakeHost::new("win32")
        .with_where(&format!("{system32}\r\n\r\n{windows_apps}\r\n{git}\r\n"))
        .with_file(system32)
        .with_file(windows_apps)
        .with_file(git);
    let value = resolve(&host).to_json();
    assert_eq!(value["found"], true);
    assert_eq!(value["source"], "path");
    assert_eq!(value["path"], git);
    assert_eq!(
        value["checkedPaths"],
        json!([GIT_BASH, PROGRAM_FILES_X86_BASH, system32, windows_apps, git])
    );
}

// --- Test 5: missing Git Bash --------------------------------------------------------------

#[tokio::test]
async fn t5_missing_git_bash_reports_hint_status_and_run_error() {
    let server = server(FakeHost::new("win32"));
    let which = server.call_tool("which_bash", None).await;
    assert!(!which.is_error);
    let value = parse(&which.text);
    assert_eq!(value["found"], false);
    assert_eq!(value["checkedPaths"], json!([GIT_BASH, PROGRAM_FILES_X86_BASH]));
    let hint = value["installHint"].as_str().expect("installHint");
    assert!(hint.contains(WINGET_INSTALL), "{hint}");
    assert!(hint.contains("OMO_CODEX_GIT_BASH_PATH"), "{hint}");
    assert!(hint.contains("bash.exe"), "{hint}");

    let diagnose = server.call_tool("diagnose", None).await;
    assert!(!diagnose.is_error);
    let report = parse(&diagnose.text);
    assert_eq!(report["platform"], "win32");
    assert_eq!(report["enabled"], false);
    assert_eq!(report["status"], "missing-git-bash");
    assert_eq!(report["resolution"], value);

    let run = server.call_tool("run", Some(&json!({"command": "ls"}))).await;
    assert!(run.is_error);
    assert_eq!(parse(&run.text), value);
    assert!(server.host().recorded().is_empty());
    assert_eq!(tool_names(&server), ["which_bash", "diagnose"]);
}

#[tokio::test]
async fn t5_tool_text_is_two_space_pretty_json() {
    let server = server(FakeHost::windows_with_git_bash());
    let reply = server.call_tool("which_bash", Some(&json!("not an object"))).await;
    let expected = serde_json::to_string_pretty(&parse(&reply.text)).expect("pretty");
    assert_eq!(reply.text, expected);
    assert!(reply.text.contains("\n  \"found\": true"), "{}", reply.text);
}

// --- Test 6: run validation ----------------------------------------------------------------

async fn run_reply(server: &Server<FakeHost>, arguments: Value) -> maho_git_bash::ToolReply {
    server.call_tool("run", Some(&arguments)).await
}

#[tokio::test]
async fn t6_invalid_arguments_are_tool_errors() {
    let server = server(FakeHost::windows_with_git_bash());
    let cases = [
        (json!({}), "command"),
        (json!({"command": "   "}), "command"),
        (json!({"command": 7}), "command"),
        (json!({"command": "ls", "workdir": ""}), "workdir"),
        (json!({"command": "ls", "workdir": 5}), "workdir"),
        (json!({"command": "ls", "cwd": "  "}), "workdir"),
        (json!({"command": "ls", "timeout": 0}), "1800000"),
        (json!({"command": "ls", "timeout": 1_800_001}), "1800000"),
        (json!({"command": "ls", "timeout": 1.5}), "1800000"),
        (json!({"command": "ls", "timeout": "abc"}), "1800000"),
        (json!({"command": "ls", "timeout": -3}), "1800000"),
        (json!({"command": "ls", "timeout_ms": true}), "1800000"),
    ];
    for (arguments, mention) in cases {
        let reply = run_reply(&server, arguments.clone()).await;
        assert!(reply.is_error, "{arguments} should be rejected");
        assert!(reply.text.contains(mention), "{arguments}: {}", reply.text);
    }
    assert!(server.host().recorded().is_empty());
}

#[tokio::test]
async fn t6_valid_arguments_reach_the_runner() {
    let server = server(FakeHost::windows_with_git_bash());
    let accepted = [
        json!({"command": "  ls -la  ", "timeout": "500", "description": "list"}),
        json!({"command": "ls", "cwd": r"C:\work", "timeout_ms": 900}),
        json!({"command": "ls", "workdir": r"C:\a", "cwd": r"C:\b", "timeout": 1800000}),
        json!({"command": "ls", "workdir": null, "cwd": r"C:\b", "timeout": null, "timeout_ms": "1"}),
        json!({"command": "ls", "timeout": 2.0}),
        json!({"command": "ls"}),
    ];
    for arguments in &accepted {
        let reply = run_reply(&server, arguments.clone()).await;
        assert!(!reply.is_error, "{arguments}: {}", reply.text);
    }
    let requests = server.host().recorded();
    let summary: Vec<_> = requests
        .iter()
        .map(|request| {
            (
                request.bash.as_str(),
                request.command.as_str(),
                request.workdir.as_deref(),
                request.timeout_ms,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (GIT_BASH, "ls -la", None, 500),
            (GIT_BASH, "ls", Some(r"C:\work"), 900),
            (GIT_BASH, "ls", Some(r"C:\a"), 1_800_000),
            (GIT_BASH, "ls", Some(r"C:\b"), 1),
            (GIT_BASH, "ls", None, 2),
            (GIT_BASH, "ls", None, FALLBACK_TIMEOUT_MS),
        ]
    );
}

#[test]
fn t6_run_schema_is_closed_and_mentions_the_default() {
    let server = Server::new(
        FakeHost::windows_with_git_bash(),
        ServerOptions {
            default_timeout_ms: Some(4321),
        },
    );
    let tools = server.tool_definitions();
    for tool in &tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
    }
    let run = &tools[0];
    assert_eq!(run["name"], "run");
    assert_eq!(run["inputSchema"]["required"], json!(["command"]));
    let description = run["inputSchema"]["properties"]["timeout"]["description"]
        .as_str()
        .expect("timeout description");
    assert!(description.contains("4321"), "{description}");
}

// --- Test 7: default timeout ---------------------------------------------------------------

fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let map: BTreeMap<String, String> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    move |name| map.get(name).cloned()
}

#[test]
fn t7_default_timeout_precedence() {
    let all = [
        ("OMO_CODEX_GIT_BASH_TIMEOUT_MS", "1000"),
        ("OMO_CODEX_EXEC_COMMAND_TIMEOUT_MS", "2000"),
        ("CODEX_EXEC_COMMAND_TIMEOUT_MS", "3000"),
        ("EXEC_COMMAND_TIMEOUT_MS", "4000"),
    ];
    assert_eq!(default_timeout_ms(Some(77), env_of(&all)), 77);
    assert_eq!(default_timeout_ms(None, env_of(&all)), 1000);
    assert_eq!(default_timeout_ms(None, env_of(&all[1..])), 2000);
    assert_eq!(default_timeout_ms(None, env_of(&all[2..])), 3000);
    assert_eq!(default_timeout_ms(None, env_of(&all[3..])), 4000);
    assert_eq!(default_timeout_ms(None, env_of(&[])), 120_000);
}

#[test]
fn t7_invalid_values_are_skipped() {
    let env = env_of(&[
        ("OMO_CODEX_GIT_BASH_TIMEOUT_MS", "soon"),
        ("OMO_CODEX_EXEC_COMMAND_TIMEOUT_MS", "0"),
        ("CODEX_EXEC_COMMAND_TIMEOUT_MS", "1800001"),
        ("EXEC_COMMAND_TIMEOUT_MS", "2500"),
    ]);
    assert_eq!(default_timeout_ms(Some(0), env), 2500);
    let none_valid = env_of(&[("EXEC_COMMAND_TIMEOUT_MS", "-1")]);
    assert_eq!(default_timeout_ms(Some(1_800_001), none_valid), 120_000);
}

#[tokio::test]
async fn t7_server_uses_host_env_for_the_default() {
    let server = server(
        FakeHost::windows_with_git_bash().with_env("CODEX_EXEC_COMMAND_TIMEOUT_MS", "6000"),
    );
    let reply = run_reply(&server, json!({"command": "ls"})).await;
    assert!(!reply.is_error);
    assert_eq!(server.host().recorded()[0].timeout_ms, 6000);
}

// --- Test 8: runner result mapping ---------------------------------------------------------

#[tokio::test]
async fn t8_non_zero_exit_is_not_a_tool_error() {
    let server = server(FakeHost::windows_with_git_bash().with_run_result(Ok(RunOutput {
        exit_code: Some(3),
        stdout: "out\n".into(),
        stderr: "err\n".into(),
        timed_out: false,
    })));
    let reply = run_reply(&server, json!({"command": "exit 3"})).await;
    assert!(!reply.is_error);
    assert_eq!(
        parse(&reply.text),
        json!({"exitCode": 3, "stdout": "out\n", "stderr": "err\n", "timedOut": false})
    );
    assert_eq!(
        reply.to_json(),
        json!({"content": [{"type": "text", "text": reply.text}], "isError": false})
    );
}

#[tokio::test]
async fn t8_missing_exit_code_is_null_and_timeout_is_reported() {
    let server = server(FakeHost::windows_with_git_bash().with_run_result(Ok(RunOutput {
        exit_code: None,
        stdout: "partial".into(),
        stderr: String::new(),
        timed_out: true,
    })));
    let reply = run_reply(&server, json!({"command": "sleep 9"})).await;
    assert!(!reply.is_error);
    let value = parse(&reply.text);
    assert_eq!(value["exitCode"], Value::Null);
    assert_eq!(value["timedOut"], true);
    assert_eq!(value["stdout"], "partial");
}

#[tokio::test]
async fn t8_runner_error_is_a_tool_error_with_its_text() {
    let server = server(
        FakeHost::windows_with_git_bash().with_run_result(Err("The system cannot find the file specified. (os error 2)".into())),
    );
    let reply = run_reply(&server, json!({"command": "ls"})).await;
    assert!(reply.is_error);
    assert_eq!(reply.text, "The system cannot find the file specified. (os error 2)");
}

#[tokio::test]
async fn t8_unknown_tool_is_a_tool_error_naming_it() {
    let server = server(FakeHost::windows_with_git_bash());
    let reply = server.call_tool("format_disk", Some(&json!({}))).await;
    assert!(reply.is_error);
    assert!(reply.text.contains("format_disk"), "{}", reply.text);
}
