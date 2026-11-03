//! Port of senpi `auth/commands-auth-dispatch.ts`: bridges the /mcp slash command to the auth flow runners
//! in commands_auth.rs, sourcing config/agentDir from the running service.
use super::commands_auth::{
    AuthCommandDeps, McpAuthNotify, McpAuthReconnect, run_auth, run_auth_complete_command,
    run_auth_start_command, run_logout_command,
};
use crate::service::McpService;
use maho_ext_api::{ExtensionUi, NotificationType};
use std::sync::Arc;

pub type McpAuthAppendEntry = Arc<dyn Fn(&str) + Send + Sync>;

/// The session pieces upstream reads from `ctx` and `pi`.
pub struct McpAuthCommandContext {
    pub ui: Arc<dyn ExtensionUi>,
    pub has_ui: bool,
    /// Upstream `pi.appendEntry("mcp-auth", message)`: with UI, auth progress is a session notice entry.
    pub append_entry: Option<McpAuthAppendEntry>,
    pub on_reconnect: McpAuthReconnect,
    pub open_browser: Option<super::challenge_login::BrowserOpener>,
}

/// The service lock is held only while the target is read: the flow's reconnect takes it again.
pub async fn handle_mcp_auth_command(
    subcommand: &str,
    args: &[String],
    ctx: McpAuthCommandContext,
    service: &tokio::sync::Mutex<McpService>,
) {
    let name = args.first().map_or("", String::as_str);
    let (target, pending, guard) = {
        let service = service.lock().await;
        (
            if name.is_empty() {
                None
            } else {
                service.auth_target(name)
            },
            service.pending_auth(),
            service.interactive_auth_guard(),
        )
    };
    let Some(target) = target else {
        ctx.ui.notify(
            &format!(
                "Unknown MCP server: {}",
                if name.is_empty() { "<missing>" } else { name }
            ),
            NotificationType::Error,
        );
        return;
    };
    let notify: McpAuthNotify = {
        let ui = ctx.ui.clone();
        let append = ctx.append_entry.clone().filter(|_| ctx.has_ui);
        Arc::new(move |message, kind| match &append {
            Some(append) => append(message),
            None => ui.notify(message, kind),
        })
    };
    let deps = AuthCommandDeps {
        server_name: name.into(),
        config: target.config,
        agent_dir: target.agent_dir,
        has_ui: ctx.has_ui,
        notify,
        callback_url: target.callback_url,
        open_browser: ctx.open_browser,
        on_reconnect: ctx.on_reconnect,
        pending,
        interactive_guard: Some(guard),
        client: reqwest::Client::new(),
    };
    let result = match subcommand {
        "auth" => run_auth(&deps).await,
        "auth-start" => run_auth_start_command(&deps).await.map(|_| ()),
        "auth-complete" => {
            let redirect = args.get(1).map_or("", String::as_str);
            if redirect.is_empty() {
                ctx.ui.notify(
                    &format!("Usage: /mcp auth-complete {name} <redirect-url>"),
                    NotificationType::Error,
                );
                return;
            }
            run_auth_complete_command(&deps, redirect).await
        }
        "logout" => run_logout_command(&deps).await,
        _ => {
            ctx.ui.notify(
                &format!("Unknown MCP auth subcommand: {subcommand}"),
                NotificationType::Error,
            );
            return;
        }
    };
    // commands.ts: a thrown flow error reaches the command's catch and is shown redacted.
    if let Err(error) = result {
        ctx.ui.notify(
            &maho_core::config_reference_redaction::redact_resolved_reference_values(
                &error.to_string(),
                "<redacted>",
            ),
            NotificationType::Error,
        );
    }
}
