//! `curated-readonly-bash.test.ts`.

use std::sync::Arc;

use crate::runners::in_process::curated_readonly_bash::{
    CuratedReadonlyBashInput, CuratedReadonlyBashTool, CuratedReadonlyCommand,
    CuratedReadonlyCommandError, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, ReadonlyProgram,
    plan_curated_readonly_command,
};

fn execute_fake_github(endpoint: &str) -> Result<String, CuratedReadonlyCommandError> {
    let endpoint_owned = endpoint.to_string();
    let tool = CuratedReadonlyBashTool::new(
        "/fixture",
        Some(Arc::new(
            move |_command: &CuratedReadonlyCommand, _cwd: &str, _timeout: u64| match endpoint_owned
                .as_str()
            {
                "lines" => Ok((0..2501)
                    .map(|index| format!("row {index}"))
                    .collect::<Vec<_>>()
                    .join("\n")),
                "bytes" => Ok("x".repeat(60_000)),
                "error" => Err(CuratedReadonlyCommandError(format!(
                    "Read-only gh request failed: {}",
                    "e".repeat(100_000)
                ))),
                _ => Ok("line one\nline two".to_string()),
            },
        )),
    );
    tool.execute(&CuratedReadonlyBashInput {
        program: ReadonlyProgram::Gh,
        args: vec!["api".to_string(), endpoint.to_string()],
        timeout_seconds: None,
    })
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn given_output_beyond_the_line_limit_when_the_tool_returns_then_it_keeps_the_head_and_reports_truncation()
 {
    let result = execute_fake_github("lines").expect("result");
    assert!(result.starts_with("row 0\nrow 1\n"));
    assert!(result.contains("[truncated:"));
    assert!(!result.contains("row 2500"));
    assert!(result.split('\n').count() <= DEFAULT_MAX_LINES);
}

#[test]
fn given_output_beyond_the_byte_limit_when_the_tool_returns_then_it_is_bounded_and_reports_truncation()
 {
    let result = execute_fake_github("bytes").expect("result");
    assert!(result.len() <= DEFAULT_MAX_BYTES);
    assert!(result.starts_with(&"x".repeat(1024)));
    assert!(result.contains("[truncated:"));
}

#[test]
fn given_failed_output_beyond_the_byte_limit_when_the_tool_rejects_then_the_diagnostic_head_is_retained_within_budget()
 {
    let message = execute_fake_github("error").expect_err("must reject").0;
    assert!(message.len() <= DEFAULT_MAX_BYTES);
    assert!(message.starts_with(&format!(
        "Read-only gh request failed: {}",
        "e".repeat(1024)
    )));
    assert!(message.contains("[truncated:"));
}

#[test]
fn given_output_within_both_limits_when_the_tool_returns_then_it_stays_byte_identical() {
    assert_eq!(
        execute_fake_github("small").expect("result"),
        "line one\nline two"
    );
}

#[test]
fn given_read_only_curl_and_github_requests_when_planned_then_direct_executables_are_returned_without_a_shell()
 {
    assert_eq!(
        plan_curated_readonly_command(
            ReadonlyProgram::Curl,
            &strings(&["--silent", "https://example.com/docs"])
        ),
        Ok(CuratedReadonlyCommand {
            program: ReadonlyProgram::Curl,
            args: strings(&["--disable", "--silent", "https://example.com/docs"]),
        })
    );
    let search = strings(&["search", "code", "createTaskEngine", "--limit", "5"]);
    assert_eq!(
        plan_curated_readonly_command(ReadonlyProgram::Gh, &search),
        Ok(CuratedReadonlyCommand {
            program: ReadonlyProgram::Gh,
            args: search.clone(),
        })
    );
}

#[test]
fn given_mutation_capable_flags_or_commands_when_planned_then_every_request_is_rejected() {
    let requests = [
        (
            ReadonlyProgram::Curl,
            strings(&["--request", "POST", "https://example.com"]),
        ),
        (
            ReadonlyProgram::Curl,
            strings(&["--output", "artifact", "https://example.com"]),
        ),
        (
            ReadonlyProgram::Curl,
            strings(&["--data", "x=1", "https://example.com"]),
        ),
        (
            ReadonlyProgram::Gh,
            strings(&["api", "repos/acme/repo", "--method", "DELETE"]),
        ),
        (
            ReadonlyProgram::Gh,
            strings(&["repo", "clone", "acme/repo"]),
        ),
    ];
    for (program, args) in requests {
        let error = plan_curated_readonly_command(program, &args).expect_err("must reject");
        assert!(error.0.contains("read-only"), "{args:?}");
    }
}
