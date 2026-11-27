use crate::tools_core_fake_machine::{ErrorMode, FakeMachine, ScriptedChunk, ScriptedExec};
use maho_machine_ops::ExecStream;
use maho_tools::bash::{
    BashExecOptions, BashExit, BashOperations, BashStream, BashToolOptions,
    create_bash_tool_definition, tools_via_daemon_env,
};
use maho_tools::definition::{AbortSignal, ToolCall, ToolFuture};
use maho_tools::machine::create_tool_machine;
use maho_tools::powershell::{PowerShellToolOptions, create_powershell_tool_definition};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn tools_via_daemon_parses_comma_separated_tool_names() {
    assert!(tools_via_daemon_env("bash", Some("bash")));
    assert!(tools_via_daemon_env("bash", Some(" read , bash , edit ")));
    assert!(!tools_via_daemon_env("bash", Some("read,write")));
    assert!(!tools_via_daemon_env("bash", None));
}

#[tokio::test]
async fn bash_via_daemon_seam_routes_bash_and_keeps_powershell_local() {
    let dir = tempfile::tempdir().expect("tempdir");

    // 1. Flag on with injected device_machine runs on device
    let fake = Arc::new(FakeMachine::new(
        HashMap::from([(
            "echo hi".to_owned(),
            ScriptedExec {
                output: vec![ScriptedChunk {
                    stream: ExecStream::Stdout,
                    text: "from-device\n".to_owned(),
                }],
                exit_code: Some(0),
                signal: None,
            },
        )]),
        ErrorMode::Fs,
        "linux",
    ));
    let device = create_tool_machine(Some(fake.clone()), None);
    let bash = create_bash_tool_definition(
        dir.path().to_path_buf(),
        BashToolOptions {
            device_machine: Some(Arc::new(move || Some(device.clone()))),
            via_daemon: Some(true),
            expose_session_environment: Some(false),
            ..Default::default()
        },
    );
    let res = (bash.execute)(ToolCall {
        id: "t1",
        params: json!({ "command": "echo hi" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("device bash");
    assert_eq!(fake.call_log(), vec!["process.exec"]);
    assert!(
        serde_json::to_string(&res)
            .expect("json")
            .contains("from-device")
    );

    // 2. Flag on with custom local operations and no device fails with DeviceUnavailableError message
    let local_runs = Arc::new(AtomicUsize::new(0));
    struct CountingOps(Arc<AtomicUsize>);
    impl BashOperations for CountingOps {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: &'a Path,
            _: BashExecOptions,
        ) -> ToolFuture<'a, BashExit> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(BashExit { exit_code: Some(0) })
            })
        }
    }
    let bash_no_device = create_bash_tool_definition(
        dir.path().to_path_buf(),
        BashToolOptions {
            via_daemon: Some(true),
            expose_session_environment: Some(false),
            operations: Some(Arc::new(CountingOps(Arc::clone(&local_runs)))),
            ..Default::default()
        },
    );
    let err = (bash_no_device.execute)(ToolCall {
        id: "t2",
        params: json!({ "command": "echo x" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect_err("expected DeviceUnavailableError");
    assert!(err.to_string().contains("device is unavailable"), "{err}");
    let cause = std::error::Error::source(&err)
        .and_then(|source| source.downcast_ref::<maho_tools::bash::DeviceUnavailableError>())
        .expect("typed device-unavailable cause");
    assert_eq!(cause.tool, "bash");
    assert_eq!(local_runs.load(Ordering::SeqCst), 0);

    // 3. Flag on still keeps powershell running on its own operations
    struct PsOps(Arc<AtomicUsize>);
    impl BashOperations for PsOps {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: &'a Path,
            options: BashExecOptions,
        ) -> ToolFuture<'a, BashExit> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                (options.on_data)(b"from-powershell\n", BashStream::Stdout)?;
                Ok(BashExit { exit_code: Some(0) })
            })
        }
    }
    let ps_runs = Arc::new(AtomicUsize::new(0));
    let ps = create_powershell_tool_definition(
        dir.path().to_path_buf(),
        PowerShellToolOptions {
            via_daemon: Some(true),
            expose_session_environment: Some(false),
            operations: Some(Arc::new(PsOps(Arc::clone(&ps_runs)))),
            ..Default::default()
        },
    );
    let ps_res = (ps.execute)(ToolCall {
        id: "t3",
        params: json!({ "command": "Write-Output x" }),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("powershell ok");
    assert_eq!(ps_runs.load(Ordering::SeqCst), 1);
    assert!(
        serde_json::to_string(&ps_res)
            .expect("json")
            .contains("from-powershell")
    );
}
