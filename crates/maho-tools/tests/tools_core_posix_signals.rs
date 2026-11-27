use crate::tools_core_fake_machine::{ErrorMode, FakeMachine, ScriptedChunk, ScriptedExec};
use maho_machine_ops::ExecStream;
use maho_tools::bash_core::{CoreBashToolOptions, create_core_bash_tool_definition};
use maho_tools::definition::{AbortSignal, ToolCall};
use maho_tools::machine::create_tool_machine;
use maho_tools::machine_output_spill::create_machine_output_spill;
use maho_tools::posix_signals::{posix_signal_number, posix_signal_table};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const COMMON_NAMES: &[&str] = &[
    "SIGHUP", "SIGINT", "SIGQUIT", "SIGILL", "SIGTRAP", "SIGABRT", "SIGFPE", "SIGKILL", "SIGSEGV",
    "SIGPIPE", "SIGALRM", "SIGTERM",
];

#[test]
fn posix_signal_table_covers_common_signals_for_every_os() {
    for os in [
        "linux", "android", "darwin", "freebsd", "openbsd", "netbsd", "win32", "aix", "sunos",
    ] {
        let missing: Vec<&str> = COMMON_NAMES
            .iter()
            .copied()
            .filter(|name| posix_signal_number(os, name).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "{os} missing common signals: {missing:?}"
        );
        assert!(!posix_signal_table(os).is_empty());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn posix_signal_table_matches_linux_os_numbers() {
    assert_eq!(posix_signal_number("linux", "SIGUSR1"), Some(10));
    assert_eq!(posix_signal_number("linux", "SIGUSR2"), Some(12));
    assert_eq!(posix_signal_number("linux", "SIGTERM"), Some(15));
    assert_eq!(posix_signal_number("linux", "SIGKILL"), Some(9));
}

#[tokio::test]
async fn worker_bash_exit_code_of_signal_killed_shell_uses_machine_os_table() {
    let cases = [
        ("linux", "SIGUSR1", 138),
        ("linux", "SIGTERM", 143),
        ("darwin", "SIGUSR1", 158),
        ("darwin", "SIGBUS", 138),
        ("win32", "SIGTERM", 143),
        ("win32", "SIGINT", 130),
        ("win32", "SIGKILL", 137),
        ("win32", "SIGABRT", 150),
        ("win32", "SIGQUIT", 131),
        ("freebsd", "SIGUSR1", 158),
        ("openbsd", "SIGTERM", 143),
        ("netbsd", "SIGUSR2", 159),
        ("aix", "SIGTERM", 143),
    ];

    for (os, signal, expected_exit_code) in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let scripts = HashMap::from([(
            "killed".to_owned(),
            ScriptedExec {
                output: vec![ScriptedChunk {
                    stream: ExecStream::Stdout,
                    text: "partial".to_owned(),
                }],
                exit_code: None,
                signal: Some(signal.to_owned()),
            },
        )]);
        let fake = Arc::new(FakeMachine::new(scripts, ErrorMode::Fs, os));
        let machine = create_tool_machine(Some(fake), None);
        let bash = create_core_bash_tool_definition(
            dir.path().to_path_buf(),
            CoreBashToolOptions {
                output_spill: Some(create_machine_output_spill(
                    machine.clone(),
                    Some(dir.path().to_path_buf()),
                )),
                ..CoreBashToolOptions::new(machine)
            },
        );
        let result = (bash.execute)(ToolCall {
            id: "call",
            params: json!({ "command": "killed" }),
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await
        .expect("execute");
        let structured = result.structured_content.expect("structured");
        assert_eq!(
            structured["exit_code"], expected_exit_code,
            "os={os} signal={signal}"
        );
        let content_json = serde_json::to_string(&result.content).expect("json");
        assert!(content_json.contains(&format!("Command exited with code {expected_exit_code}")));
    }
}
