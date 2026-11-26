//! Port of `packages/coding-agent/src/core/tools/bash.ts`.

pub use crate::bash_core::resolve_spawn_context;
pub use crate::bash_schema::{
    BASH_PROMPT_GUIDELINE, BASH_PROMPT_SHORT_GUIDELINE, BASH_PROMPT_SNIPPET, BashSpawnContext,
    BashSpawnHook, BashToolDetails, BashToolInput, ShellEnvProvider, ShellToolConfig,
    bash_tool_config,
};
pub use crate::shell_operations::{
    BashDataCallback, BashExecOptions, BashExit, BashOperations, BashStream, resolve_timeout_ms,
};

use crate::bash_core::{ShellToolCoreOptions, create_shell_tool_core};
use crate::definition::{ToolDefinition, ToolError, ToolExposure, ToolFuture};
use crate::machine::{ToolMachine, default_tool_machine};
use crate::output_accumulator::create_temp_file_spill;
use crate::shell_operations::{ShellExecHost, create_shell_operations};
use maho_machine_ops::{EnvMode, ExecShell};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type ShellConfigResolver =
    Arc<dyn Fn() -> Result<crate::shell::ShellConfig, ToolError> + Send + Sync>;

pub type DeviceMachineProvider = Arc<dyn Fn() -> Option<ToolMachine> + Send + Sync>;

/// A tool routed to the session's device by `OMO_TOOLS_VIA_DAEMON` found no device to run on;
/// it never falls back to local.
#[derive(Debug, Clone, thiserror::Error)]
#[error(
    "{tool}: the session's device is unavailable, and OMO_TOOLS_VIA_DAEMON routes {tool} there, so it does not run locally"
)]
pub struct DeviceUnavailableError {
    pub tool: String,
}

impl DeviceUnavailableError {
    pub const CODE: &'static str = "device_unavailable";
    pub fn new(tool: impl Into<String>) -> Self {
        Self { tool: tool.into() }
    }
}

pub fn tools_via_daemon_env(tool: &str, value: Option<&str>) -> bool {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .any(|name| name == tool)
}

pub fn tools_via_daemon(tool: &str) -> bool {
    let raw = std::env::var("OMO_TOOLS_VIA_DAEMON").ok();
    tools_via_daemon_env(tool, raw.as_deref())
}

fn local_shell_host(
    resolve_shell_config: ShellConfigResolver,
    machine: ToolMachine,
) -> ShellExecHost {
    let os_machine = machine;
    ShellExecHost {
        resolve_shell: Arc::new(move || {
            let cfg = resolve_shell_config()?;
            Ok(Some(ExecShell {
                path: cfg.shell,
                args: cfg.args,
                command_transport: if cfg.command_transport == Some("stdin") {
                    maho_machine_ops::CommandTransport::Stdin
                } else {
                    maho_machine_ops::CommandTransport::Argv
                },
            }))
        }),
        check_cwd: Arc::new(|cwd: &Path, _signal| {
            let cwd = cwd.to_path_buf();
            Box::pin(async move {
                tokio::fs::metadata(&cwd)
                    .await
                    .map(|_| ())
                    .map_err(|e| ToolError::Message(e.to_string()))
            })
        }),
        env_mode: EnvMode::Exact,
        default_env: Arc::new(|| Some(std::env::vars().collect())),
        signal_number: Arc::new(move |sig| {
            let os = os_machine.ops().info().host().os;
            crate::posix_signals::posix_signal_number(&os, sig)
        }),
        on_spawn: Some(Arc::new(|pid| {
            if let Ok(pid) = i32::try_from(pid) {
                crate::shell::track_detached_child_pid(pid);
            }
        })),
        on_exit: Some(Arc::new(|pid| {
            if let Ok(pid) = i32::try_from(pid) {
                crate::shell::note_detached_child_exited(pid);
            }
        })),
        attest_session_spawn: None,
        after_exec: Some(Arc::new(crate::shell::prune_tracked_detached_children)),
    }
}

/// Shared process execution used by the built-in shell tools (`process.exec` via `ToolMachine`).
pub fn create_local_shell_operations_with_machine(
    shell_name: &str,
    resolve_shell_config: ShellConfigResolver,
    machine: Option<ToolMachine>,
) -> Arc<dyn BashOperations> {
    let machine = machine.unwrap_or_else(default_tool_machine);
    let host = local_shell_host(resolve_shell_config, machine.clone());
    create_shell_operations(shell_name, machine, host)
}

pub fn create_local_shell_operations(
    shell_name: &str,
    resolve_shell_config: ShellConfigResolver,
) -> Arc<dyn BashOperations> {
    create_local_shell_operations_with_machine(shell_name, resolve_shell_config, None)
}

pub fn create_local_bash_operations(shell: Option<&str>) -> Arc<dyn BashOperations> {
    create_local_bash_operations_with_machine(shell, None)
}

pub fn create_local_bash_operations_with_machine(
    shell: Option<&str>,
    machine: Option<ToolMachine>,
) -> Arc<dyn BashOperations> {
    let shell_path = shell.map(str::to_owned);
    create_local_shell_operations_with_machine(
        "bash",
        Arc::new(move || {
            crate::shell::get_shell_config(shell_path.as_deref()).map_err(ToolError::Message)
        }),
        machine,
    )
}

struct DeviceRoutedBashOperations {
    tool_name: String,
    shell_path: Option<String>,
    resolve_device: DeviceMachineProvider,
}

impl BashOperations for DeviceRoutedBashOperations {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        cwd: &'a Path,
        options: BashExecOptions,
    ) -> ToolFuture<'a, BashExit> {
        Box::pin(async move {
            let Some(device) = (self.resolve_device)() else {
                return Err(ToolError::DeviceUnavailable(DeviceUnavailableError::new(
                    &self.tool_name,
                )));
            };
            create_local_bash_operations_with_machine(self.shell_path.as_deref(), Some(device))
                .exec(command, cwd, options)
                .await
        })
    }
}

#[derive(Default, Clone)]
pub struct BashToolOptions {
    /// Redact credential-shaped output before streaming or spilling. Default: false.
    pub redact_secrets_in_output: bool,
    pub operations: Option<Arc<dyn BashOperations>>,
    pub command_prefix: Option<String>,
    pub shell_path: Option<String>,
    pub expose_session_environment: Option<bool>,
    pub spawn_hook: Option<BashSpawnHook>,
    pub shell_env: Option<ShellEnvProvider>,
    /// SAFETY-DIVERGENCE(senpi tools/bash.ts): `Some(passthrough)` scrubs secret-looking variables from
    /// the final spawn environment, keeping the listed names; `None` leaves the environment untouched.
    pub env_passthrough: Option<Vec<String>>,
    /// Opt-in OS write-confinement (`bashSandbox`); `None` is the same as `Off`.
    pub sandbox: Option<crate::bash_sandbox::BashSandboxMode>,
    /// Agent dir that holds the sandbox's writable `sessions`/`cache` and private `tmp/bash-<session>` dirs.
    pub sandbox_agent_dir: Option<PathBuf>,
    /// Operation layer and policy gate. Default: the process-wide local machine.
    pub machine: Option<ToolMachine>,
    /// Session device machine used only when `OMO_TOOLS_VIA_DAEMON` names `"bash"`.
    pub device_machine: Option<DeviceMachineProvider>,
    /// Test override for `OMO_TOOLS_VIA_DAEMON=bash`; `None` checks the environment variable.
    pub via_daemon: Option<bool>,
}

pub fn create_shell_tool_definition(
    cwd: PathBuf,
    config: ShellToolConfig,
    options: BashToolOptions,
) -> ToolDefinition {
    let machine = options.machine.unwrap_or_else(default_tool_machine);
    let via_device = config.name == "bash"
        && options
            .via_daemon
            .unwrap_or_else(|| tools_via_daemon("bash"));
    let server_pinned = options.operations.is_some() || options.spawn_hook.is_some();

    let ops: Arc<dyn BashOperations> = if via_device {
        let resolve_device = options.device_machine.unwrap_or_else(|| Arc::new(|| None));
        Arc::new(DeviceRoutedBashOperations {
            tool_name: config.name.clone(),
            shell_path: options.shell_path.clone(),
            resolve_device,
        })
    } else {
        options.operations.unwrap_or_else(|| {
            create_local_bash_operations_with_machine(
                options.shell_path.as_deref(),
                Some(machine.clone()),
            )
        })
    };

    let shell_env = options
        .shell_env
        .unwrap_or_else(|| Arc::new(|| std::env::vars().collect::<BTreeMap<String, String>>()));

    let mut def = create_shell_tool_core(
        cwd,
        config,
        ShellToolCoreOptions {
            machine,
            operations: ops,
            output_spill: create_temp_file_spill(),
            shell_env: Some(shell_env),
            annotate_trust: None,
            redact_secrets_in_output: options.redact_secrets_in_output,
            command_prefix: options.command_prefix,
            expose_session_environment: options.expose_session_environment,
            spawn_hook: options.spawn_hook,
            env_passthrough: options.env_passthrough,
            sandbox: options.sandbox,
            sandbox_agent_dir: options.sandbox_agent_dir,
        },
    );
    def.exposure = Some(ToolExposure::Eval);
    if server_pinned {
        def.server_pinned = Some(true);
    }
    def
}

pub fn create_bash_tool_definition(cwd: PathBuf, options: BashToolOptions) -> ToolDefinition {
    create_shell_tool_definition(cwd, bash_tool_config(), options)
}

#[cfg(test)]
mod browser_env_tests {
    use super::*;
    use crate::definition::{ToolContext, ToolSessionManager};
    use std::path::Path;

    #[test]
    fn browser_engine_is_removed_from_inherited_shell_environment() {
        let shell_env: ShellEnvProvider =
            Arc::new(|| BTreeMap::from([("OMO_BROWSER_ENGINE".into(), "stale".into())]));
        let resolved = resolve_spawn_context(
            "true".into(),
            PathBuf::from("."),
            None,
            false,
            None,
            Some(&shell_env),
        )
        .expect("resolve shell context");
        assert!(!resolved.env.contains_key("OMO_BROWSER_ENGINE"));
    }

    struct BrowserContext {
        session: Session,
    }
    struct Session;
    impl ToolSessionManager for Session {
        fn session_id(&self) -> &str {
            "test"
        }
        fn session_file(&self) -> Option<&Path> {
            None
        }
    }
    impl ToolContext for BrowserContext {
        fn cwd(&self) -> &Path {
            Path::new(".")
        }
        fn model(&self) -> Option<&maho_ai::model::Model> {
            None
        }
        fn thinking_level(&self) -> Option<maho_ai::types::ThinkingLevel> {
            None
        }
        fn session_manager(&self) -> &dyn ToolSessionManager {
            &self.session
        }
        fn goal_store_file(&self) -> Option<&Path> {
            None
        }
        fn browser_engine(&self) -> Option<&str> {
            Some("chromium")
        }
    }

    #[test]
    fn browser_engine_is_exported_only_when_session_environment_is_exposed() {
        let context = BrowserContext { session: Session };
        let hidden = resolve_spawn_context(
            "true".into(),
            PathBuf::from("."),
            None,
            false,
            Some(&context),
            None,
        )
        .expect("resolve hidden context");
        let exposed = resolve_spawn_context(
            "true".into(),
            PathBuf::from("."),
            None,
            true,
            Some(&context),
            None,
        )
        .expect("resolve exposed context");
        assert!(!hidden.env.contains_key("OMO_BROWSER_ENGINE"));
        assert_eq!(
            exposed.env.get("OMO_BROWSER_ENGINE").map(String::as_str),
            Some("chromium")
        );
    }
}

#[cfg(test)]
mod shell_lifecycle_tests {
    use super::*;
    use crate::definition::{AbortSignal, ToolCall};
    use serde_json::json;
    use std::sync::Mutex;
    use std::time::Duration;

    struct RetainedOutput {
        callback: Mutex<Option<BashDataCallback>>,
        exit_code: Option<i32>,
    }
    impl BashOperations for RetainedOutput {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: &'a Path,
            options: BashExecOptions,
        ) -> ToolFuture<'a, BashExit> {
            Box::pin(async move {
                (options.on_data)(b"initial", BashStream::Stdout)?;
                *self.callback.lock().expect("callback lock") = Some(options.on_data);
                Ok(BashExit {
                    exit_code: self.exit_code,
                })
            })
        }
    }

    #[tokio::test]
    async fn nonzero_exit_returns_error_status_with_structured_output() {
        let tool = create_bash_tool_definition(
            ".".into(),
            BashToolOptions {
                operations: Some(Arc::new(RetainedOutput {
                    callback: Mutex::new(None),
                    exit_code: Some(7),
                })),
                ..Default::default()
            },
        );
        let wrapped = crate::tool_definition_wrapper::wrap_tool_definition(tool, None, None);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            (wrapped.execute)("nonzero".into(), json!({"command":"ignored"}), None, None),
        )
        .await
        .expect("bounded shell execution");
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("structured shell result");
        assert_eq!(structured["exit_code"], 7);
        assert_eq!(structured["output"], "initial");
    }

    #[tokio::test]
    async fn retained_callback_ignores_output_after_completion() {
        let operations = Arc::new(RetainedOutput {
            callback: Mutex::new(None),
            exit_code: Some(0),
        });
        let tool = create_bash_tool_definition(
            ".".into(),
            BashToolOptions {
                operations: Some(operations.clone()),
                ..Default::default()
            },
        );
        let result = (tool.execute)(ToolCall {
            id: "late",
            params: json!({"command":"ignored"}),
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await
        .expect("execute");
        let callback = operations
            .callback
            .lock()
            .expect("callback lock")
            .take()
            .expect("retained callback");
        assert!(callback(b"late", BashStream::Stdout).is_ok());
        assert_eq!(
            result.structured_content.expect("structured")["output"],
            "initial"
        );
    }

    #[tokio::test]
    async fn custom_null_exit_is_an_execution_failure() {
        let tool = create_bash_tool_definition(
            ".".into(),
            BashToolOptions {
                operations: Some(Arc::new(RetainedOutput {
                    callback: Mutex::new(None),
                    exit_code: None,
                })),
                ..Default::default()
            },
        );
        let result = (tool.execute)(ToolCall {
            id: "null",
            params: json!({"command":"ignored"}),
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await;
        assert!(matches!(result, Err(ToolError::Message(_))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_signal_exit_uses_shell_status() {
        let operations = create_local_bash_operations(None);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            operations.exec(
                "kill -TERM $$",
                Path::new("."),
                BashExecOptions {
                    on_data: Arc::new(|_, _| Ok(())),
                    redact_secrets_in_output: false,
                    signal: AbortSignal::default(),
                    timeout: None,
                    env: Some(std::env::vars().collect()),
                    session_id: None,
                },
            ),
        )
        .await
        .expect("bounded command")
        .expect("execute");
        assert_eq!(result.exit_code, Some(143));
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::*;
    use crate::definition::{AbortSignal, ToolCall, ToolContent};
    use serde_json::json;

    struct TwoStreams;
    impl BashOperations for TwoStreams {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: &'a Path,
            options: BashExecOptions,
        ) -> ToolFuture<'a, BashExit> {
            Box::pin(async move {
                assert!(options.redact_secrets_in_output);
                (options.on_data)(b"TOKEN=abc", BashStream::Stdout)?;
                (options.on_data)(b"err line\n", BashStream::Stderr)?;
                (options.on_data)(b"def\nok\n", BashStream::Stdout)?;
                Ok(BashExit { exit_code: Some(0) })
            })
        }
    }

    #[tokio::test]
    async fn redacts_per_stream_and_flushes_pending_tokens_before_the_result() {
        let tool = create_bash_tool_definition(
            ".".into(),
            BashToolOptions {
                redact_secrets_in_output: true,
                operations: Some(Arc::new(TwoStreams)),
                ..Default::default()
            },
        );
        let result = (tool.execute)(ToolCall {
            id: "redact",
            params: json!({"command": "x"}),
            signal: AbortSignal::default(),
            on_update: None,
            context: None,
        })
        .await
        .expect("execute");
        let text = match &result.content[0] {
            ToolContent::Text { text, .. } => text.clone(),
            _ => panic!("text"),
        };
        assert!(text.contains("TOKEN=<redacted>"), "{text}");
        assert!(!text.contains("abcdef"), "{text}");
        assert!(text.contains("err line"), "{text}");
        assert!(text.contains("ok"), "{text}");
    }
}
