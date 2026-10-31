//! Port of senpi `commands.ts`: the `/mcp` slash command.
use crate::{
    auth::commands_auth_dispatch::{McpAuthCommandContext, handle_mcp_auth_command},
    config_schema::{McpServerConfig, Transport},
    service::McpService,
};
use maho_ext_api::{ExtensionApi, ExtensionContext, NotificationType};
use std::sync::Arc;

pub const SUBCOMMANDS: [&str; 11] = [
    "status",
    "add",
    "enable",
    "disable",
    "test",
    "logs",
    "reconnect",
    "auth",
    "auth-start",
    "auth-complete",
    "logout",
];
const AUTH_SUBCOMMANDS: [&str; 4] = ["auth", "auth-start", "auth-complete", "logout"];
pub const MCP_AUTH_ENTRY_TYPE: &str = "mcp-auth";

/// What the `/mcp` handler needs from the session beyond its command context.
#[derive(Clone)]
pub struct McpCommandHost {
    pub service: Arc<tokio::sync::Mutex<McpService>>,
    /// Upstream `pendingAttach`: every subcommand waits for the single in-flight attach.
    pub pending_attach: Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>,
    /// Upstream `pi.appendEntry("mcp-auth", ...)`.
    pub append_entry: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// Runs after `/mcp reconnect` so wire status follows the reconnect.
    pub after_reconnect:
        Option<Arc<dyn Fn(crate::service_types::McpWireStatusSnapshot) + Send + Sync>>,
    pub open_browser: Option<crate::auth::challenge_login::BrowserOpener>,
}

/// Upstream `registerMcpCommands`.
pub fn register_mcp_commands(api: &mut ExtensionApi, host: McpCommandHost) {
    api.register_entry_renderer(
        MCP_AUTH_ENTRY_TYPE,
        maho_ext_host::notice::notice_entry_renderer(|entry| {
            let data = entry.data.get("data").unwrap_or(&entry.data);
            data.as_str().map(|why| maho_ext_host::notice::NoticeSpec {
                title: "MCP authorization".into(),
                tone: None,
                why: why.into(),
                extra: Vec::new(),
                expanded_line: None,
            })
        }),
        Default::default(),
    );
    api.register_command_with_completions(
        "mcp",
        Some("Inspect and manage MCP servers.".into()),
        None,
        Arc::new(move |raw, ctx| {
            let host = host.clone();
            Box::pin(async move {
                // Startup attach no longer blocks the first frame, so /mcp can be reached while it is still
                // in flight; every subcommand reports or mutates attached state.
                (host.pending_attach)().await;
                match split_command_args(raw) {
                    Ok(args) => handle_mcp_command(&args, ctx, &host).await,
                    Err(error) => ctx.ui.notify(
                        &maho_core::config_reference_redaction::redact_resolved_reference_values(
                            &error.to_string(),
                            "<redacted>",
                        ),
                        NotificationType::Error,
                    ),
                }
                Ok(())
            })
        }),
        Arc::new(|prefix| {
            Box::pin(async move {
                Ok(Some(
                    complete_mcp_subcommands(prefix)
                        .into_iter()
                        .map(|value| maho_ext_api::AutocompleteItem {
                            value: value.into(),
                            label: value.into(),
                            description: None,
                        })
                        .collect(),
                ))
            })
        }),
    );
}

/// Upstream `getArgumentCompletions`.
pub fn complete_mcp_subcommands(prefix: &str) -> Vec<&'static str> {
    SUBCOMMANDS
        .into_iter()
        .filter(|item| item.starts_with(prefix))
        .collect()
}

fn redact(text: &str) -> String {
    maho_core::config_reference_redaction::redact_resolved_reference_values(text, "<redacted>")
}

/// Upstream `handleMcpCommand`.
pub async fn handle_mcp_command(args: &[String], ctx: &ExtensionContext, host: &McpCommandHost) {
    let subcommand = args.first().map_or("", String::as_str);
    let name = args.get(1).map_or("", String::as_str);
    let ui = ctx.ui.clone();
    if subcommand.is_empty() {
        if !ctx.has_ui || ctx.mode != maho_ext_api::ExtensionMode::Tui {
            let text = host.service.lock().await.status("MCP servers").await;
            ui.notify(&text, NotificationType::Info);
        } else if let Err(error) = crate::manager::show_mcp_manager(ctx, host).await {
            ui.notify(&redact(&error.message), NotificationType::Error);
        }
        return;
    }
    if AUTH_SUBCOMMANDS.contains(&subcommand) {
        run_auth_command(subcommand, &args[1..], ctx, host).await;
        return;
    }
    match subcommand {
        "status" => {
            let text = host.service.lock().await.status("MCP status").await;
            ui.notify(&text, NotificationType::Info);
        }
        "add" => add_server(&args[1..], ctx, host).await,
        "enable" | "disable" => set_server_enabled(ctx, host, name, subcommand == "enable").await,
        "test" => test_server(name, ctx, host, None).await,
        "logs" => show_logs(name, ctx, host).await,
        "reconnect" => reconnect_server(name, ctx, host, None).await,
        _ => ui.notify(
            &format!("Unknown /mcp subcommand: {subcommand}"),
            NotificationType::Error,
        ),
    }
}

/// A notice sink replacing `ctx.ui.notify` (upstream `Notify`).
pub type McpNotify = Arc<dyn Fn(&str, NotificationType) + Send + Sync>;

/// The commands the manager dispatches through the existing handlers (upstream `McpManagerCommand`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpManagerCommand {
    Test,
    Reconnect,
    Auth,
    Logout,
}

/// Upstream manager `run` callback: `handleMcpCommand([command, name], ctx, pi, service, notify)`.
pub async fn run_manager_command(
    command: McpManagerCommand,
    name: &str,
    ctx: &ExtensionContext,
    host: &McpCommandHost,
    notify: Option<McpNotify>,
) {
    let args = [name.to_owned()];
    match command {
        McpManagerCommand::Test => test_server(name, ctx, host, notify).await,
        McpManagerCommand::Reconnect => reconnect_server(name, ctx, host, notify).await,
        McpManagerCommand::Auth => run_auth_command("auth", &args, ctx, host).await,
        McpManagerCommand::Logout => run_auth_command("logout", &args, ctx, host).await,
    }
}

async fn run_auth_command(
    subcommand: &str,
    args: &[String],
    ctx: &ExtensionContext,
    host: &McpCommandHost,
) {
    let ui = ctx.ui.clone();
    let name = args.first().map_or("", String::as_str);
    {
        let reconnect_service = host.service.clone();
        let reconnect_name = name.to_owned();
        let reconnect_ctx = ReattachContext::from(ctx);
        handle_mcp_auth_command(
            subcommand,
            args,
            McpAuthCommandContext {
                ui,
                has_ui: ctx.has_ui,
                append_entry: host.append_entry.clone(),
                on_reconnect: Arc::new(move || {
                    let service = reconnect_service.clone();
                    let name = reconnect_name.clone();
                    let ctx = reconnect_ctx.clone();
                    Box::pin(async move {
                        // A needs_auth/degraded state after logout is expected; status shows it.
                        let mut service = service.lock().await;
                        let _ = service.reconnect_server(&name).await;
                        let _ = ctx.reattach(&mut service).await;
                    })
                }),
                open_browser: host.open_browser.clone(),
            },
            &host.service,
        )
        .await;
    }
}

/// The reload attach a command runs (`service.attachSession({type:"session_start",reason:"reload"}, ctx, pi)`).
#[derive(Clone)]
pub(crate) struct ReattachContext {
    cwd: std::path::PathBuf,
    agent_dir: std::path::PathBuf,
    project_trusted: bool,
    declarations: Vec<maho_ext_api::RegisteredMcpServerDeclaration>,
}
impl From<&ExtensionContext> for ReattachContext {
    fn from(ctx: &ExtensionContext) -> Self {
        Self {
            cwd: ctx.cwd.clone(),
            agent_dir: ctx.agent_dir.clone(),
            project_trusted: ctx.is_project_trusted(),
            declarations: ctx.registered_mcp_servers.clone(),
        }
    }
}
impl ReattachContext {
    pub(crate) async fn reattach(
        &self,
        service: &mut McpService,
    ) -> Result<(), crate::service::McpServiceError> {
        let env = std::env::vars().collect();
        service
            .attach_session(
                &self.cwd,
                &self.agent_dir,
                &env,
                self.project_trusted,
                &self.declarations,
            )
            .await
    }
}

async fn add_server(args: &[String], ctx: &ExtensionContext, host: &McpCommandHost) {
    let ui = &ctx.ui;
    let (Some(name), Some(_)) = (args.first(), args.get(1)) else {
        ui.notify(
            "Usage: /mcp add <name> <command...|url>",
            NotificationType::Error,
        );
        return;
    };
    let endpoint = &args[1..];
    if !ctx.has_ui {
        ui.notify(
            "Cannot add MCP server without UI confirmation.",
            NotificationType::Error,
        );
        return;
    }
    let server = parse_server_config(endpoint);
    if !ui
        .confirm(
            "Add MCP server?",
            &format!("{name}: {}", endpoint.join(" ")),
            Default::default(),
        )
        .await
    {
        ui.notify("MCP add cancelled", NotificationType::Warning);
        return;
    }
    if let Err(error) = crate::config_edit::add_global_mcp_server(&ctx.agent_dir, name, &server) {
        ui.notify(&redact(&error.to_string()), NotificationType::Error);
        return;
    }
    let mut service = host.service.lock().await;
    if let Err(error) = ReattachContext::from(ctx).reattach(&mut service).await {
        ui.notify(&redact(&error.to_string()), NotificationType::Error);
        return;
    }
    ui.notify(&format!("Added MCP server {name}"), NotificationType::Info);
}

async fn set_server_enabled(
    ctx: &ExtensionContext,
    host: &McpCommandHost,
    name: &str,
    enabled: bool,
) {
    let ui = &ctx.ui;
    let mut service = host.service.lock().await;
    if !ensure_known(name, &service, &|text| {
        ui.notify(text, NotificationType::Error)
    }) {
        return;
    }
    match crate::config_edit::set_global_mcp_server_enabled(&ctx.agent_dir, name, enabled) {
        Err(error) => {
            ui.notify(&redact(&error.to_string()), NotificationType::Error);
            return;
        }
        Ok(false) => {
            ui.notify(
                &format!("MCP server {name} is not in the global config file"),
                NotificationType::Error,
            );
            return;
        }
        Ok(true) => {}
    }
    ui.notify(
        &format!("MCP server {name} connecting"),
        NotificationType::Info,
    );
    if let Err(error) = ReattachContext::from(ctx).reattach(&mut service).await {
        ui.notify(&redact(&error.to_string()), NotificationType::Error);
        return;
    }
    ui.notify(
        &format!(
            "{} MCP server {name}",
            if enabled { "Enabled" } else { "Disabled" }
        ),
        NotificationType::Info,
    );
}

fn notifier(ctx: &ExtensionContext, notify: Option<McpNotify>) -> McpNotify {
    notify.unwrap_or_else(|| {
        let ui = ctx.ui.clone();
        Arc::new(move |text, kind| ui.notify(text, kind))
    })
}

async fn test_server(
    name: &str,
    ctx: &ExtensionContext,
    host: &McpCommandHost,
    notify: Option<McpNotify>,
) {
    let notify = notifier(ctx, notify);
    let service = host.service.lock().await;
    if !ensure_known(name, &service, &|text| {
        notify(text, NotificationType::Error)
    }) {
        return;
    }
    match service.test_server(name).await {
        Ok((elapsed, tools)) => notify(
            &format!("MCP test {name} ok ({elapsed:.0}ms): {tools} tools"),
            NotificationType::Info,
        ),
        Err(failure) => notify(
            &redact(&format!(
                "MCP test {name} failed ({:.0}ms): {}",
                failure.elapsed_ms, failure.error
            )),
            NotificationType::Error,
        ),
    }
}

async fn show_logs(name: &str, ctx: &ExtensionContext, host: &McpCommandHost) {
    let ui = &ctx.ui;
    let service = host.service.lock().await;
    if !ensure_known(name, &service, &|text| {
        ui.notify(text, NotificationType::Error)
    }) {
        return;
    }
    let lines = service.log_lines(name, 20).await;
    ui.notify(
        &if lines.is_empty() {
            format!("MCP logs for {name}: (empty)")
        } else {
            lines.join("\n")
        },
        NotificationType::Info,
    );
}

async fn reconnect_server(
    name: &str,
    ctx: &ExtensionContext,
    host: &McpCommandHost,
    notify: Option<McpNotify>,
) {
    let notify = notifier(ctx, notify);
    let mut service = host.service.lock().await;
    if !ensure_known(name, &service, &|text| {
        notify(text, NotificationType::Error)
    }) {
        return;
    }
    let result = match service.reconnect_server(name).await {
        Ok(()) => ReattachContext::from(ctx).reattach(&mut service).await,
        Err(error) => Err(error),
    };
    if let Some(after) = &host.after_reconnect {
        after(service.wire_status_snapshot().await);
    }
    match result {
        Ok(()) => notify(
            &format!("MCP reconnect {name} connected"),
            NotificationType::Info,
        ),
        Err(error) => notify(
            &redact(&format!("MCP reconnect {name} failed: {error}")),
            NotificationType::Error,
        ),
    }
}

/// Upstream `ensureKnown`.
fn ensure_known(name: &str, service: &McpService, notify: &dyn Fn(&str)) -> bool {
    let known = service.server_names();
    if !name.is_empty() && known.iter().any(|known| known == name) {
        return true;
    }
    let known = known.join(", ");
    notify(&format!(
        "Unknown MCP server: {}\nKnown MCP servers: {}",
        if name.is_empty() { "<missing>" } else { name },
        if known.is_empty() { "(none)" } else { &known }
    ));
    false
}

pub fn parse_server_config(endpoint: &[String]) -> McpServerConfig {
    let first = endpoint.first().cloned().unwrap_or_default();
    crate::config::normalize_server(
        if first.starts_with("http://") || first.starts_with("https://") {
            McpServerConfig {
                transport: Some(Transport::Http),
                url: Some(first),
                ..Default::default()
            }
        } else {
            McpServerConfig {
                transport: Some(Transport::Stdio),
                command: Some(first),
                args: Some(endpoint.iter().skip(1).cloned().collect()),
                ..Default::default()
            }
        },
    )
}
pub fn split_command_args(raw: &str) -> Result<Vec<String>, regex::Error> {
    let matcher =
        regex::Regex::new(r#""([^"\\]*(?:\\.[^"\\]*)*)"|'([^'\\]*(?:\\.[^'\\]*)*)'|[^\s]+"#)?;
    let escape = regex::Regex::new(r#"\\(["'])"#)?;
    Ok(matcher
        .find_iter(raw)
        .map(|part| {
            let part = part.as_str();
            let part = part.strip_prefix(['"', '\'']).unwrap_or(part);
            let part = part.strip_suffix(['"', '\'']).unwrap_or(part);
            escape.replace_all(part, "$1").into_owned()
        })
        .collect())
}
