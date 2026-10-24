//! Integration tests for the opt-in bash sandbox. They spawn real commands, so they run only where
//! the platform tool exists (Linux: bwrap, macOS: sandbox-exec). With `MHC_REQUIRE_SANDBOX_TEST=1`
//! (CI) a missing tool fails the test instead of skipping it.
#![cfg(any(target_os = "linux", target_os = "macos"))]
use maho_tools::{
    bash::*,
    bash_sandbox::{BashSandboxMode, find_in_path},
    definition::*,
};
use serde_json::json;
use std::path::{Path, PathBuf};

fn tool_name() -> &'static str {
    if cfg!(target_os = "linux") {
        "bwrap"
    } else {
        "sandbox-exec"
    }
}
/// True when the test may run; panics on CI when the tool is missing.
fn available() -> bool {
    if find_in_path(tool_name()).is_some() {
        return true;
    }
    assert!(
        std::env::var("MHC_REQUIRE_SANDBOX_TEST").as_deref() != Ok("1"),
        "{} is required for sandbox tests (MHC_REQUIRE_SANDBOX_TEST=1)",
        tool_name()
    );
    false
}

struct Env {
    _root: tempfile::TempDir,
    _home: tempfile::TempDir,
    project: PathBuf,
    outside: PathBuf,
    agent_dir: PathBuf,
}
/// `project` and `outside` are siblings under a temp root that is not any writable root; the agent
/// dir (so `<private_tmp>`) lives under a separate temp HOME.
fn env(project_name: &str) -> Env {
    // Not under /tmp: bwrap masks /tmp with a tmpfs, which would make `outside` simply vanish
    // instead of being read-only confined via `--ro-bind / /`.
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp");
    std::fs::create_dir_all(&base).unwrap();
    let root = tempfile::tempdir_in(&base).unwrap();
    let home = tempfile::tempdir_in(&base).unwrap();
    let project = std::fs::canonicalize(root.path())
        .unwrap()
        .join(project_name);
    let outside = std::fs::canonicalize(root.path()).unwrap().join("outside");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let agent_dir = std::fs::canonicalize(home.path()).unwrap().join("agent");
    Env {
        _root: root,
        _home: home,
        project,
        outside,
        agent_dir,
    }
}

fn tool(cwd: &Path, mode: BashSandboxMode, agent_dir: &Path) -> ToolDefinition {
    create_bash_tool_definition(
        cwd.into(),
        BashToolOptions {
            sandbox: Some(mode),
            sandbox_agent_dir: Some(agent_dir.into()),
            ..Default::default()
        },
    )
}
async fn run(
    tool: &ToolDefinition,
    command: &str,
    signal: AbortSignal,
) -> Result<ToolResult, ToolError> {
    (tool.execute)(ToolCall {
        id: "sandbox",
        params: json!({"command": command}),
        signal,
        on_update: None,
        context: None,
    })
    .await
}
fn output(result: &ToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            ToolContent::Text { text, .. } => Some(text.as_str()),
            ToolContent::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn exit_code(result: &ToolResult) -> i64 {
    result.structured_content.as_ref().unwrap()["exit_code"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn sandbox_blocks_outside_writes_but_allows_inside() {
    if !available() {
        return;
    }
    let e = env("proj");
    let command = format!("echo x > {}/probe", e.outside.display());
    let off = tool(&e.project, BashSandboxMode::Off, &e.agent_dir);
    let result = run(&off, &command, AbortSignal::default()).await.unwrap();
    assert_eq!(exit_code(&result), 0);
    assert!(
        e.outside.join("probe").exists(),
        "unwrapped command must write outside"
    );
    std::fs::remove_file(e.outside.join("probe")).unwrap();

    let required = tool(&e.project, BashSandboxMode::Required, &e.agent_dir);
    let result = run(&required, &command, AbortSignal::default())
        .await
        .unwrap();
    assert_ne!(exit_code(&result), 0, "{}", output(&result));
    assert!(!e.outside.join("probe").exists());
    let visible = run(
        &required,
        &format!("test -d {0} && test -r {0}", e.outside.display()),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        exit_code(&visible),
        0,
        "outside must be visible read-only in the sandbox: {}",
        output(&visible)
    );
    let result = run(
        &required,
        "echo x > inside/probe 2>/dev/null || (mkdir inside && echo x > inside/probe)",
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(exit_code(&result), 0, "{}", output(&result));
    assert!(e.project.join("inside/probe").exists());
}

#[tokio::test]
async fn sandbox_exports_private_tmpdir() {
    if !available() {
        return;
    }
    let e = env("proj");
    let required = tool(&e.project, BashSandboxMode::Required, &e.agent_dir);
    let result = run(
        &required,
        "echo x > \"$TMPDIR/probe\" && printf %s \"$TMPDIR\"",
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(exit_code(&result), 0, "{}", output(&result));
    let private_tmp = PathBuf::from(output(&result).lines().next().unwrap());
    assert!(private_tmp.starts_with(e.agent_dir.join("tmp")));
    assert!(private_tmp.join("probe").exists());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandbox_tmp_is_private_tmpfs_on_linux() {
    if !available() {
        return;
    }
    let e = env("proj");
    let name = format!("mhc-probe-{}", std::process::id());
    let required = tool(&e.project, BashSandboxMode::Required, &e.agent_dir);
    let result = run(
        &required,
        &format!("echo x > /tmp/{name}"),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(exit_code(&result), 0, "{}", output(&result));
    assert!(
        !Path::new("/tmp").join(&name).exists(),
        "host /tmp must not see sandbox writes"
    );
}

#[tokio::test]
async fn sandbox_handles_spaces_and_quotes_in_paths() {
    if !available() {
        return;
    }
    let e = env("my proj 'q' \"d\"");
    let required = tool(&e.project, BashSandboxMode::Required, &e.agent_dir);
    let result = run(
        &required,
        "echo x > ok.txt && echo it\\'s",
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(exit_code(&result), 0, "{}", output(&result));
    assert!(e.project.join("ok.txt").exists());
}

#[tokio::test]
async fn sandbox_follows_symlinked_cwd() {
    if !available() {
        return;
    }
    let e = env("proj");
    let link = e.outside.join("link");
    std::os::unix::fs::symlink(&e.project, &link).unwrap();
    let required = tool(&link, BashSandboxMode::Required, &e.agent_dir);
    let result = run(&required, "echo x > viaLink.txt", AbortSignal::default())
        .await
        .unwrap();
    assert_eq!(exit_code(&result), 0, "{}", output(&result));
    assert!(e.project.join("viaLink.txt").exists());
    let result = run(
        &required,
        &format!("echo x > {}/probe", e.outside.display()),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_ne!(exit_code(&result), 0);
}

#[tokio::test]
async fn sandbox_propagates_exit_code() {
    if !available() {
        return;
    }
    let e = env("proj");
    let required = tool(&e.project, BashSandboxMode::Required, &e.agent_dir);
    let result = run(&required, "exit 7", AbortSignal::default())
        .await
        .unwrap();
    assert_eq!(exit_code(&result), 7);
}

#[tokio::test]
async fn sandbox_cancel_propagates() {
    if !available() {
        return;
    }
    let e = env("proj");
    let required = tool(&e.project, BashSandboxMode::Required, &e.agent_dir);
    let signal = AbortSignal::default();
    let abort = signal.clone();
    let started = e.project.join("started");
    let watcher = tokio::spawn(async move {
        while !started.exists() {
            tokio::task::yield_now().await;
        }
        abort.abort();
    });
    let error = run(&required, "touch started; sleep 60", signal)
        .await
        .err()
        .unwrap();
    watcher.await.unwrap();
    assert!(error.to_string().contains("Command aborted"), "{error}");
}

#[tokio::test]
async fn sandbox_adversarial_command_stays_confined() {
    if !available() {
        return;
    }
    let e = env("proj");
    let required = tool(&e.project, BashSandboxMode::Required, &e.agent_dir);
    let out = e.outside.display();
    let command =
        format!("echo \"it's\" \\\\ x\n$(touch {out}/x)\ntouch {out}/y; echo 'a'\\''b' > {out}/z");
    let visible = run(
        &required,
        &format!("test -d {out} && test -r {out}"),
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(exit_code(&visible), 0, "{}", output(&visible));
    let _ = run(&required, &command, AbortSignal::default())
        .await
        .unwrap();
    for name in ["x", "y", "z"] {
        assert!(!e.outside.join(name).exists(), "{name} escaped the sandbox");
    }
}

#[tokio::test]
async fn sandbox_blocks_system_path_writes() {
    if !available() {
        return;
    }
    let e = env("proj");
    let required = tool(&e.project, BashSandboxMode::Required, &e.agent_dir);
    let result = run(&required, "echo x > /etc/mhc-probe", AbortSignal::default())
        .await
        .unwrap();
    assert_ne!(exit_code(&result), 0, "{}", output(&result));
    assert!(!Path::new("/etc/mhc-probe").exists());
}

#[tokio::test]
async fn sandbox_composes_with_env_scrub() {
    if !available() {
        return;
    }
    let e = env("proj");
    let shell_env: ShellEnvProvider = std::sync::Arc::new(|| {
        std::collections::BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("GITHUB_TOKEN".to_owned(), "secret".to_owned()),
        ])
    });
    let definition = create_bash_tool_definition(
        e.project.clone(),
        BashToolOptions {
            sandbox: Some(BashSandboxMode::Required),
            sandbox_agent_dir: Some(e.agent_dir.clone()),
            env_passthrough: Some(vec![]),
            shell_env: Some(shell_env),
            ..Default::default()
        },
    );
    let result = run(
        &definition,
        "printf '%s|%s' \"${GITHUB_TOKEN-unset}\" \"$TMPDIR\"",
        AbortSignal::default(),
    )
    .await
    .unwrap();
    assert_eq!(exit_code(&result), 0, "{}", output(&result));
    let text = output(&result);
    assert!(text.starts_with("unset|"), "{text}");
    assert!(text.contains(&e.agent_dir.join("tmp").to_string_lossy().into_owned()));
}
