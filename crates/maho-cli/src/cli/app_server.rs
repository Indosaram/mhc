//! Port of senpi `cli/app-server-command.ts` (`handleAppServerCommand`) and the process-entry half
//! of `modes/app-server/index.ts` (`runAppServerMode`): argument parsing, daemon dispatch, the
//! listening banner and the SIGINT/SIGTERM escalation.
//!
//! The transports, the runtime and the daemon lifecycle are owned by todo 37
//! (`maho_server::app_server`); this module owns the CLI entry that mounts them.

use std::path::Path;
use std::sync::Arc;

use maho_server::app_server::cli_args::{format_usage, parse_cli_args, CliArgs, DaemonVerb, Listen, WsAuth};
use maho_server::app_server::daemon::{run_daemon_command, DaemonPaths};
use maho_server::app_server::index::run_app_server_mode;
use maho_server::app_server::runtime::AppServerRuntime;
use maho_server::app_server::thread_registry::SessionFactory;

use super::host_runtime::{mount_agent_session_runtime, CliRuntimeConfiguration, CliRuntimeRequest};

pub const APP_SERVER_COMMAND: &str = "app-server";

/// senpi `process.exit(2)` for a usage error.
pub const USAGE_EXIT_CODE: i32 = 2;
/// senpi `process.exitCode = 1` for a failed daemon command.
pub const ERROR_EXIT_CODE: i32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalEscalation {
    /// First SIGINT/SIGTERM: request a graceful shutdown.
    First,
    /// Second signal: the pinned `requestShutdown` force-exits with code 1.
    Forced,
}

#[derive(Default)]
pub struct ShutdownSignals {
    requested: bool,
    forced: bool,
}

impl ShutdownSignals {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(&mut self) -> SignalEscalation {
        if self.requested {
            self.forced = true;
            SignalEscalation::Forced
        } else {
            self.requested = true;
            SignalEscalation::First
        }
    }

    pub fn requested(&self) -> bool {
        self.requested
    }

    pub fn forced(&self) -> bool {
        self.forced
    }
}

pub fn is_app_server_command(args: &[String]) -> bool {
    args.first().is_some_and(|arg| arg == APP_SERVER_COMMAND)
}

pub struct AppServerOutcome {
    pub handled: bool,
    pub exit_code: i32,
}

/// The app-server session factory: each thread mounts through the CLI runtime factory, so an
/// app-server session and a CLI session are built by the same code path.
pub fn session_factory(config: CliRuntimeConfiguration) -> SessionFactory {
    Arc::new(move |options: maho_core::sdk::CreateAgentSessionOptions| {
        let config = config.clone();
        Box::pin(async move {
            let maho_core::sdk::CreateAgentSessionOptions {
                session_manager,
                model_runtime,
                settings_manager,
                model,
                thinking_level,
                thinking_selection,
                scoped_models,
                favorite_models,
                tools,
                exclude_tools,
                no_tools,
                custom_tools,
                session_start_event,
                auto_title_sessions,
                ..
            } = options;
            let request = CliRuntimeRequest {
                session_manager,
                session_start_event,
                model,
                thinking_level,
                thinking_selection,
                scoped_models,
                favorite_models,
                tools,
                exclude_tools,
                no_tools,
                custom_tools,
                auto_title_sessions,
                ..Default::default()
            };
            let mounted = mount_agent_session_runtime(&config, request, model_runtime, settings_manager).await?;
            Ok(mounted.session().clone())
        })
    })
}

fn write_stderr(message: &str) {
    use std::io::Write;
    if let Err(error) = std::io::stderr().lock().write_all(message.as_bytes()) {
        eprintln!("{error}");
    }
}

/// senpi `runAppServerMode`'s listening banner, written to stderr before the shutdown wait.
pub fn listening_banner(app_name: &str, listen: &Listen) -> String {
    match listen {
        Listen::Stdio { .. } => format!("{app_name} app-server listening on stdio://\n"),
        Listen::Ws { host, port, .. } => format!(
            "{app_name} app-server listening on ws://{host}:{port}\nreadyz http://127.0.0.1:{port}/readyz\n"
        ),
        Listen::Unix { path, url } => {
            let socket = path.clone().unwrap_or_else(|| url.clone());
            format!("{app_name} app-server listening on unix://{socket}\n")
        }
    }
}

/// senpi `handleAppServerCommand` plus the server-mode process entry.
///
/// `shutdown_signal` is awaited once the first SIGINT/SIGTERM arrives; the caller installs the
/// escalation task that drives [`ShutdownSignals`].
pub async fn run_app_server_command(
    args: &[String],
    config: CliRuntimeConfiguration,
    version: &str,
    session_dir: Option<String>,
    executable: &Path,
    prefix_args: &[String],
    shutdown_signal: impl std::future::Future<Output = ()>,
) -> Result<AppServerOutcome, String> {
    if !is_app_server_command(args) {
        return Ok(AppServerOutcome { handled: false, exit_code: 0 });
    }
    let app_name = maho_core::config::app_name();
    match parse_cli_args(&args[1..]) {
        CliArgs::UsageError { message } => {
            write_stderr(&format!("Error: {message}\n{}\n", format_usage(&app_name)));
            Ok(AppServerOutcome { handled: true, exit_code: USAGE_EXIT_CODE })
        }
        CliArgs::Daemon { verb, listen } => {
            let paths = DaemonPaths::new(Path::new(&config.agent_dir));
            let listen_value = serde_json::to_value(&listen).map_err(|error| error.to_string())?;
            let output = run_daemon_command(&paths, verb, &listen_value, version, executable, prefix_args).await;
            match output {
                Ok(payload) => {
                    println!("{}", serde_json::to_string(&payload).map_err(|error| error.to_string())?);
                    Ok(AppServerOutcome { handled: true, exit_code: 0 })
                }
                Err(error) => {
                    println!(
                        "{}",
                        serde_json::json!({ "status": "error", "message": error.to_string() })
                    );
                    Ok(AppServerOutcome { handled: true, exit_code: ERROR_EXIT_CODE })
                }
            }
        }
        CliArgs::Server { listen, ws_auth, json_logs } => {
            let _ = json_logs;
            let runtime = AppServerRuntime::new(
                config.agent_dir.clone(),
                config.cwd.clone(),
                version.to_owned(),
                session_dir,
                Some(session_factory(config.clone())),
            )
            .await;
            write_stderr(&listening_banner(&app_name, &listen));
            let auth: Option<WsAuth> = ws_auth;
            let result = run_app_server_mode(&runtime, listen, auth, shutdown_signal).await;
            let exit_code = match result {
                Ok(()) => 0,
                Err(error) => {
                    write_stderr(&format!("Error: {error}\n"));
                    ERROR_EXIT_CODE
                }
            };
            Ok(AppServerOutcome { handled: true, exit_code })
        }
    }
}

/// The daemon verb the CLI is asked to run, for callers that must decide before dispatch.
pub fn daemon_verb(args: &[String]) -> Option<DaemonVerb> {
    match parse_cli_args(&args[1..]) {
        CliArgs::Daemon { verb, .. } => Some(verb),
        _ => None,
    }
}

/// Process-entry wrapper: install the pinned SIGINT/SIGTERM escalation, then run the command.
#[cfg(unix)]
pub async fn run_app_server_with_signals(
    args: &[String],
    config: CliRuntimeConfiguration,
    version: &str,
    session_dir: Option<String>,
    executable: &Path,
    prefix_args: &[String],
) -> Result<i32, String> {
    let mut signals = ShutdownSignals::new();
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let mut sender = Some(sender);
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|error| error.to_string())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| error.to_string())?;
    tokio::spawn(async move {
        loop {
            tokio::select! { _ = interrupt.recv() => {}, _ = terminate.recv() => {} }
            match signals.request() {
                SignalEscalation::First => {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(());
                    }
                }
                SignalEscalation::Forced => std::process::exit(1),
            }
        }
    });
    let shutdown = async move {
        let _ = receiver.await;
    };
    let outcome =
        run_app_server_command(args, config, version, session_dir, executable, prefix_args, shutdown).await?;
    Ok(if outcome.handled { outcome.exit_code } else { 0 })
}

/// Non-unix hosts have no SIGINT/SIGTERM signal set; the command runs without the escalation task.
#[cfg(not(unix))]
pub async fn run_app_server_with_signals(
    args: &[String],
    config: CliRuntimeConfiguration,
    version: &str,
    session_dir: Option<String>,
    executable: &Path,
    prefix_args: &[String],
) -> Result<i32, String> {
    let outcome = run_app_server_command(args, config, version, session_dir, executable, prefix_args, std::future::pending()).await?;
    Ok(if outcome.handled { outcome.exit_code } else { 0 })
}
