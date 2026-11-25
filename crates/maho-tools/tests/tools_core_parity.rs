use crate::tools_core_fake_machine::{ErrorMode, FakeMachine, ScriptedChunk, ScriptedExec};
use maho_machine_ops::ExecStream;
use maho_tools::bash::{BashToolOptions, create_bash_tool_definition};
use maho_tools::bash_core::{CoreBashToolOptions, create_core_bash_tool_definition};
use maho_tools::coding_tools_core::{CoreCodingToolsOptions, create_core_coding_tools};
use maho_tools::definition::{AbortSignal, ToolCall, ToolDefinition, ToolError, ToolFuture};
use maho_tools::edit::{EditToolOptions, create_edit_tool_definition};
use maho_tools::edit_core::{
    CoreEditToolOptions, EditOperations, create_core_edit_tool_definition,
};
use maho_tools::index::{
    ALL_TOOL_NAMES, ToolsOptions, create_all_tool_definitions, create_coding_tools,
};
use maho_tools::machine::{create_tool_machine, default_tool_machine};
use maho_tools::machine_output_spill::create_machine_output_spill;
use maho_tools::read::{ReadToolOptions, SelectedReadFolder, create_read_tool_definition};
use maho_tools::read_core::{CoreReadToolOptions, create_core_read_tool_definition};
use maho_tools::write::{WriteToolOptions, create_write_tool_definition};
use maho_tools::write_core::{CoreWriteToolOptions, create_core_write_tool_definition};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

fn model_facing_json(def: &ToolDefinition) -> Value {
    json!({
        "name": def.name,
        "label": def.label,
        "description": def.description,
        "parameters": def.parameters,
        "promptSnippet": def.prompt_snippet,
        "promptGuidelines": def.prompt_guidelines,
        "outputSchema": def.output_schema,
        "exposure": def.exposure,
        "constrainedSampling": def.constrained_sampling,
        "workspaceEffect": def.workspace_effect,
    })
}

#[test]
fn tools_core_definitions_expose_local_model_facing_fields() {
    let machine = default_tool_machine();
    let local = create_all_tool_definitions(Path::new("/workspace"), ToolsOptions::default());
    let core = [
        (
            "bash",
            create_core_bash_tool_definition(
                "/workspace".into(),
                CoreBashToolOptions::new(machine.clone()),
            ),
        ),
        (
            "read",
            create_core_read_tool_definition(
                "/workspace".into(),
                CoreReadToolOptions::new(machine.clone()),
            ),
        ),
        (
            "edit",
            create_core_edit_tool_definition(
                "/workspace".into(),
                CoreEditToolOptions::new(machine.clone()),
            ),
        ),
        (
            "write",
            create_core_write_tool_definition(
                "/workspace".into(),
                CoreWriteToolOptions::new(machine),
            ),
        ),
    ];
    for (name, core_def) in core {
        let local_def = &local[name];
        assert_eq!(
            model_facing_json(&core_def),
            model_facing_json(local_def),
            "model-facing mismatch for {name}"
        );
        assert_eq!(
            core_def.prepare_arguments.is_some(),
            local_def.prepare_arguments.is_some()
        );
    }
    assert!(ALL_TOOL_NAMES.contains(&"bash"));
}

#[test]
fn create_core_coding_tools_matches_order_of_create_coding_tools() {
    let machine = default_tool_machine();
    let core_names: Vec<String> = create_core_coding_tools(
        Path::new("/workspace"),
        CoreCodingToolsOptions::new(machine),
    )
    .into_iter()
    .map(|t| t.name().to_owned())
    .collect();
    let local_names: Vec<String> =
        create_coding_tools(Path::new("/workspace"), ToolsOptions::default())
            .into_iter()
            .map(|t| t.name().to_owned())
            .collect();
    assert_eq!(core_names, local_names);
}

fn parity_scripts() -> HashMap<String, ScriptedExec> {
    let long_output: String = (1..=3000).map(|i| format!("{i}\n")).collect();
    HashMap::from([
        (
            "long output".to_owned(),
            ScriptedExec {
                output: vec![ScriptedChunk {
                    stream: ExecStream::Stdout,
                    text: long_output,
                }],
                exit_code: Some(0),
                signal: None,
            },
        ),
        (
            "fails".to_owned(),
            ScriptedExec {
                output: vec![ScriptedChunk {
                    stream: ExecStream::Stderr,
                    text: "boom\n".to_owned(),
                }],
                exit_code: Some(2),
                signal: None,
            },
        ),
        (
            "killed".to_owned(),
            ScriptedExec {
                output: vec![ScriptedChunk {
                    stream: ExecStream::Stdout,
                    text: "partial".to_owned(),
                }],
                exit_code: None,
                signal: Some("SIGUSR1".to_owned()),
            },
        ),
    ])
}

fn seed(cwd: &Path) {
    std::fs::create_dir_all(cwd).expect("mkdir");
    let notes: String = (0..2500)
        .map(|i| format!("line {} \u{00e9}\u{4e2d}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(cwd.join("notes.txt"), notes).expect("write notes.txt");
    let app: String = (0..150)
        .map(|i| format!("function f{i}() {{\n\treturn {i};\n}}"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(cwd.join("app.js"), app).expect("write app.js");
    std::fs::write(
        cwd.join("edit-me.txt"),
        "\u{feff}alpha\r\nbeta\r\ngamma\r\n",
    )
    .expect("write edit-me.txt");
    std::fs::write(cwd.join("same.txt"), "unchanged\n").expect("write same.txt");
}

fn normalize_outcome(
    val: Result<maho_tools::definition::ToolResult, ToolError>,
    cwd: &Path,
    dir: &Path,
) -> Value {
    let mut raw = match val {
        Ok(res) => json!({ "result": res }),
        Err(err) => json!({ "error": err.to_string() }),
    };
    let s = serde_json::to_string(&raw).expect("serialize");
    let log_re = regex::Regex::new(r"/[^\s\]\x22]*pi-bash-[^.\x22]+\.log").expect("regex");
    let s = log_re.replace_all(&s, "<SPILL>");
    let cwd_s = cwd.to_string_lossy();
    let dir_s = dir.to_string_lossy();
    let s = s
        .replace(cwd_s.as_ref(), "<CWD>")
        .replace(dir_s.as_ref(), "<DIR>");
    let wall_re = regex::Regex::new(r#""wall_time_seconds":[0-9.]+"#).expect("regex");
    let s = wall_re
        .replace_all(&s, r#""wall_time_seconds":0"#)
        .into_owned();
    raw = serde_json::from_str(&s).expect("reparse");
    raw
}

async fn run_call(
    def: &ToolDefinition,
    params: Value,
) -> Result<maho_tools::definition::ToolResult, ToolError> {
    (def.execute)(ToolCall {
        id: "call",
        params,
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
}

#[tokio::test]
async fn parity_between_core_and_local_tools_across_fs_and_op_error_modes() {
    for error_mode in [ErrorMode::Fs, ErrorMode::Op] {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        let fake = Arc::new(FakeMachine::new(
            parity_scripts(),
            error_mode,
            std::env::consts::OS,
        ));
        let machine = create_tool_machine(Some(fake.clone()), None);
        let core_cwd = dir.join("core");
        let local_cwd = dir.join("local");
        seed(&core_cwd);
        seed(&local_cwd);

        // 1. bash parity cases
        let core_bash = create_core_bash_tool_definition(
            core_cwd.clone(),
            CoreBashToolOptions {
                output_spill: Some(create_machine_output_spill(
                    machine.clone(),
                    Some(dir.to_path_buf()),
                )),
                ..CoreBashToolOptions::new(machine.clone())
            },
        );
        let local_bash = create_bash_tool_definition(
            local_cwd.clone(),
            BashToolOptions {
                machine: Some(machine.clone()),
                ..Default::default()
            },
        );
        for cmd in ["long output", "fails", "killed"] {
            let c_out = normalize_outcome(
                run_call(&core_bash, json!({"command": cmd})).await,
                &core_cwd,
                dir,
            );
            let l_out = normalize_outcome(
                run_call(&local_bash, json!({"command": cmd})).await,
                &local_cwd,
                dir,
            );
            assert_eq!(c_out, l_out, "bash parity mismatch for {cmd}");
        }

        // 2. read parity cases
        let core_read = create_core_read_tool_definition(
            core_cwd.clone(),
            CoreReadToolOptions {
                folder: Some(Arc::new(SelectedReadFolder)),
                ..CoreReadToolOptions::new(machine.clone())
            },
        );
        let local_read = create_read_tool_definition(
            local_cwd.clone(),
            ReadToolOptions {
                machine: Some(machine.clone()),
                ..Default::default()
            },
        );
        for params in [
            json!({"path": "notes.txt", "offset": 100, "limit": 20}),
            json!({"path": "notes.txt"}),
            json!({"path": "app.js"}),
            json!({"path": "missing.txt"}),
        ] {
            let c_out =
                normalize_outcome(run_call(&core_read, params.clone()).await, &core_cwd, dir);
            let l_out =
                normalize_outcome(run_call(&local_read, params.clone()).await, &local_cwd, dir);
            assert_eq!(c_out, l_out, "read parity mismatch for {params}");
        }

        // 3. write parity cases
        let core_write = create_core_write_tool_definition(
            core_cwd.clone(),
            CoreWriteToolOptions::new(machine.clone()),
        );
        let local_write = create_write_tool_definition(
            local_cwd.clone(),
            WriteToolOptions {
                machine: Some(machine.clone()),
                ..Default::default()
            },
        );
        for (rel, params) in [
            (
                "out/new.txt",
                json!({"path": "out/new.txt", "content": "hello\nworld\n"}),
            ),
            (
                "notes.txt",
                json!({"path": "notes.txt", "content": "short\n"}),
            ),
            (
                "same.txt",
                json!({"path": "same.txt", "content": "unchanged\n"}),
            ),
        ] {
            let c_out =
                normalize_outcome(run_call(&core_write, params.clone()).await, &core_cwd, dir);
            let l_out = normalize_outcome(
                run_call(&local_write, params.clone()).await,
                &local_cwd,
                dir,
            );
            assert_eq!(c_out, l_out, "write parity mismatch for {rel}");
            assert_eq!(
                std::fs::read(core_cwd.join(rel)).expect("core written"),
                std::fs::read(local_cwd.join(rel)).expect("local written"),
            );
        }

        // 4. edit parity cases
        let core_edit = create_core_edit_tool_definition(
            core_cwd.clone(),
            CoreEditToolOptions::new(machine.clone()),
        );
        let local_edit = create_edit_tool_definition(
            local_cwd.clone(),
            EditToolOptions {
                machine: Some(machine.clone()),
                ..Default::default()
            },
        );
        for params in [
            json!({"path": "edit-me.txt", "edits": [{"oldText": "beta", "newText": "BETA"}]}),
            json!({"path": "edit-me.txt", "edits": [{"oldText": "delta", "newText": "DELTA"}]}),
            json!({"path": "missing.txt", "edits": [{"oldText": "a", "newText": "b"}]}),
        ] {
            let c_out =
                normalize_outcome(run_call(&core_edit, params.clone()).await, &core_cwd, dir);
            let l_out =
                normalize_outcome(run_call(&local_edit, params.clone()).await, &local_cwd, dir);
            assert_eq!(c_out, l_out, "edit parity mismatch for {params}");
            assert_eq!(
                std::fs::read(core_cwd.join("edit-me.txt")).expect("core edit-me"),
                std::fs::read(local_cwd.join("edit-me.txt")).expect("local edit-me"),
            );
        }

        assert!(!fake.call_log().is_empty());
    }
}

#[tokio::test]
async fn edit_preserves_fs_error_codes_from_wrapped_raw_os_errors() {
    struct WrappedErrOps(i32);
    impl EditOperations for WrappedErrOps {
        fn read_file<'a>(&'a self, _: &'a Path) -> ToolFuture<'a, Vec<u8>> {
            Box::pin(async move { Err(ToolError::Io(std::io::Error::from_raw_os_error(self.0))) })
        }
        fn write_file<'a>(&'a self, _: &'a Path, _: &'a str) -> ToolFuture<'a, ()> {
            Box::pin(async move { Err(ToolError::Io(std::io::Error::from_raw_os_error(self.0))) })
        }
        fn access<'a>(&'a self, _: &'a Path) -> ToolFuture<'a, ()> {
            Box::pin(async move { Err(ToolError::Io(std::io::Error::from_raw_os_error(self.0))) })
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let fake = Arc::new(FakeMachine::new(HashMap::new(), ErrorMode::Fs, "linux"));
    let machine = create_tool_machine(Some(fake), None);

    #[cfg(unix)]
    let cases = [
        (nix::errno::Errno::EISDIR as i32, "EISDIR"),
        (nix::errno::Errno::ENOTDIR as i32, "ENOTDIR"),
        (nix::errno::Errno::EPERM as i32, "EPERM"),
    ];
    #[cfg(not(unix))]
    let cases: [(i32, &str); 0] = [];

    for (raw, expected) in cases {
        let edit = create_core_edit_tool_definition(
            dir.path().to_path_buf(),
            CoreEditToolOptions {
                operations: Some(Arc::new(WrappedErrOps(raw))),
                ..CoreEditToolOptions::new(machine.clone())
            },
        );
        let err = run_call(
            &edit,
            json!({"path": "x.txt", "edits": [{"oldText": "a", "newText": "b"}]}),
        )
        .await
        .expect_err("expected error");
        assert!(
            err.to_string().contains(&format!(
                "Could not edit file: x.txt. Error code: {expected}."
            )),
            "got: {err}"
        );
    }
}
