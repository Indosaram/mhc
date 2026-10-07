//! Port of the pinned step-up re-login: `mcp-oauth/provider.ts::login` plus
//! `skill-mcp-manager/manager.ts::withOperationRetry`'s `forceReconnect`.
use std::path::{Path,PathBuf};
use std::sync::{Arc,Weak};
use crate::config_schema::McpServerConfig;
use crate::errors::{McpError,McpErrorKind};
use crate::transport_sdk::{AuthStepUpLoginFuture,AuthStepUpReconnectFuture,McpAuthStepUp};
/// Pinned `oauth-handler.ts::ignoreOAuthFallbackError`: an OAuth failure is swallowed so the caller
/// can fall back to the post-request refresh.
fn step_up_failure(server:&str,message:impl std::fmt::Display)->McpError {
    let mut failure=McpError::new(McpErrorKind::Auth,format!("MCP server {server} step-up re-authentication failed: {message}"));
    failure.phase=Some("step-up".into());failure.server_name=Some(server.into());failure
}
/// The production step-up handler: a real interactive `login()` through
/// `commands_auth::run_interactive_login` followed by a real `ServerConnection::renew`.
pub struct McpInteractiveStepUp {
    server_name:String,
    config:McpServerConfig,
    agent_dir:PathBuf,
    port:Option<u16>,
    /// Production `true`; the deterministic loopback fixtures opt out through the provider seam.
    pub require_https:bool,
    browser:Arc<dyn Fn(&str)+Send+Sync>,
    client:reqwest::Client,
    connection:Weak<crate::connection::ServerConnection>,
}
impl McpInteractiveStepUp {
    pub fn new(server_name:&str,config:&McpServerConfig,agent_dir:&Path,port:Option<u16>,connection:Weak<crate::connection::ServerConnection>)->Self {
        Self {server_name:server_name.into(),config:config.clone(),agent_dir:agent_dir.into(),port,require_https:true,browser:Arc::new(super::commands_auth::open_browser),client:reqwest::Client::new(),connection}
    }
    /// Override the pinned `openBrowser` seam so the deterministic loopback fixtures drive the
    /// callback leg themselves instead of launching a real browser.
    pub fn set_browser(&mut self,browser:Arc<dyn Fn(&str)+Send+Sync>) {self.browser=browser;}
}
impl McpAuthStepUp for McpInteractiveStepUp {
    fn login(&self,required_scopes:Vec<String>)->AuthStepUpLoginFuture {
        // Pinned `handleStepUpIfNeeded`: `config.oauth.scopes = mergeScopes(current, required)` and a
        // fresh provider built from that config (pinned `authProviders.delete(url)` +
        // `getOrCreateAuthProvider(...)`), rather than mutating the provider behind its `Arc`.
        let mut config=self.config.clone();
        let mut oauth=config.oauth.clone().unwrap_or_default();
        oauth.scopes=Some(super::step_up::merge_scopes(oauth.scopes.as_deref().unwrap_or(&[]),&required_scopes));
        config.oauth=Some(oauth);
        let name=self.server_name.clone();
        let agent_dir=self.agent_dir.clone();
        let port=self.port;
        let require_https=self.require_https;
        let browser=self.browser.clone();
        let client=self.client.clone();
        let ui=self.connection.upgrade().and_then(|connection|connection.elicitation_ui());
        Box::pin(async move {
            let authorization_name=name.clone();
            let on_authorization=move|url:url::Url|{let browser=browser.clone();let authorization_name=authorization_name.clone();async move {
                // Pinned `oauth-authorization-flow.ts::openBrowser`.
                browser(url.as_str());
                if let Some(ui)=ui.as_ref() {ui.notify(&format!("Open this URL to authorize {authorization_name}:\n{url}"),maho_ext_api::NotificationType::Info);}
                Ok::<(),super::oauth::OAuthRequestError>(())
            }};
            super::commands_auth::run_interactive_login(super::commands_auth::InteractiveLoginOptions {name:&name,config:&config,agent_dir:&agent_dir,callback_url:None,port,force:true,require_https,client:&client},on_authorization).await
                .map_err(|error|step_up_failure(&name,error))
        })
    }
    fn reconnect(&self)->AuthStepUpReconnectFuture {
        let server_name=self.server_name.clone();
        let connection=self.connection.clone();
        Box::pin(async move {
            let connection=connection.upgrade().ok_or_else(||step_up_failure(&server_name,"the server connection was disposed before the step-up reconnect"))?;
            // Pinned `manager.ts::withOperationRetry` -> `forceReconnect(state, clientKey)`.
            connection.renew().await?;
            connection.client()
        })
    }
}
