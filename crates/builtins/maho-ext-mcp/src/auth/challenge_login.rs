//! Production auth challenge handler: when a server asks for more scopes, run a forced interactive
//! loopback login with the merged scopes, then renew the owning connection.
use std::sync::{Arc, Weak};

use futures::future::BoxFuture;

use super::{
    challenge::McpAuthChallengeHandler,
    commands_auth::{build_provider, run_loopback_auth_forced},
    oauth::OAuthRequestError,
    oauth_provider::McpOAuthProvider,
};
use crate::{
    config_schema::McpServerConfig,
    connection::ServerConnection,
    errors::{McpError, McpErrorKind},
    transport_sdk::McpClient,
};

/// Opens an authorization URL in the user's browser. Injectable so tests can follow the redirect.
pub type BrowserOpener = Arc<dyn Fn(&url::Url) -> Result<(), String> + Send + Sync>;
/// Builds a fresh provider for one login run (the loopback flow consumes its provider).
pub type ProviderFactory = Arc<dyn Fn() -> Result<McpOAuthProvider, OAuthRequestError> + Send + Sync>;

/// Opens the URL with the platform's default handler.
pub fn system_browser_opener() -> BrowserOpener {
    Arc::new(|url| {
        let target = url.as_str();
        #[cfg(target_os = "macos")]
        let mut command = std::process::Command::new("open");
        #[cfg(target_os = "macos")]
        command.arg(target);
        #[cfg(windows)]
        let mut command = std::process::Command::new("rundll32");
        #[cfg(windows)]
        command.args(["url.dll,FileProtocolHandler", target]);
        #[cfg(not(any(target_os = "macos", windows)))]
        let mut command = std::process::Command::new("xdg-open");
        #[cfg(not(any(target_os = "macos", windows)))]
        command.arg(target);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

pub struct InteractiveChallengeHandler {
    factory: ProviderFactory,
    connection: Weak<ServerConnection>,
    opener: BrowserOpener,
    client: reqwest::Client,
    port: Option<u16>,
}

impl InteractiveChallengeHandler {
    pub fn new(
        factory: ProviderFactory,
        connection: Weak<ServerConnection>,
        opener: BrowserOpener,
        port: Option<u16>,
    ) -> Self {
        Self {
            factory,
            connection,
            opener,
            client: reqwest::Client::new(),
            port,
        }
    }
    /// Production wiring for a configured OAuth server; opens the system browser.
    pub fn for_server(
        name: &str,
        config: &McpServerConfig,
        agent_dir: &std::path::Path,
        connection: Weak<ServerConnection>,
    ) -> Self {
        let port = config
            .oauth
            .as_ref()
            .filter(|oauth| oauth.client_id.is_some() && oauth.client_metadata_url.is_none())
            .and_then(|oauth| oauth.callback_port);
        let (name, config, agent_dir) = (name.to_owned(), config.clone(), agent_dir.to_path_buf());
        let factory: ProviderFactory =
            Arc::new(move || build_provider(&name, &config, &agent_dir, None));
        Self::new(factory, connection, system_browser_opener(), port)
    }
}

fn auth_error(error: impl std::fmt::Display) -> McpError {
    let mut failure = McpError::new(McpErrorKind::Auth, error.to_string());
    failure.phase = Some("request".into());
    failure
}

impl McpAuthChallengeHandler for InteractiveChallengeHandler {
    fn reauthorize(&self, scopes: Vec<String>) -> BoxFuture<'_, Result<(), McpError>> {
        let factory = self.factory.clone();
        let connection = self.connection.clone();
        let opener = self.opener.clone();
        let client = self.client.clone();
        let port = self.port;
        Box::pin(async move {
            let provider = factory().map_err(auth_error)?;
            provider.escalate_scopes(&scopes);
            let ui = connection
                .upgrade()
                .and_then(|connection| connection.elicitation_ui());
            run_loopback_auth_forced(provider, port, &client, |url| async move {
                if let Some(ui) = &ui {
                    ui.notify(
                        &format!("This MCP server needs more permissions. Open this URL to authorize:\n{url}"),
                        maho_ext_api::NotificationType::Info,
                    );
                }
                // A failed launch is not fatal: the notice above carries the URL.
                let _ = opener(&url);
                Ok(())
            })
            .await
            .map_err(auth_error)
        })
    }
    fn renew(&self) -> BoxFuture<'_, Result<Arc<McpClient>, McpError>> {
        let connection = self.connection.clone();
        Box::pin(async move {
            let connection = connection
                .upgrade()
                .ok_or_else(|| auth_error("MCP connection is gone"))?;
            connection.renew().await
        })
    }
}
