pub struct ProjectMcpMaterialization {
    pub config: crate::config_schema::McpServerConfig,
    pub env: std::collections::BTreeMap<String, String>,
}
pub fn register_project_policy(config_hash: String, resolver: ProjectPolicyResolver) {
    PROJECT_POLICIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(config_hash, resolver);
}
pub fn project_policy(config_hash: &str) -> Option<ProjectPolicyResolver> {
    PROJECT_POLICIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(config_hash)
        .cloned()
}
static PROJECT_POLICIES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::BTreeMap<String, ProjectPolicyResolver>>,
> = std::sync::LazyLock::new(Default::default);
pub type ProjectPolicyResolver = std::sync::Arc<
    dyn Fn() -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<ProjectMcpMaterialization, crate::errors::McpError>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;
use crate::{
    config_schema::{Auth, AuthMode, McpServerConfig, Transport},
    errors::{McpError, McpErrorKind},
    transport_sdk::McpTransportSpec,
};
use crate::{log::McpLogger, transport_sdk::McpClient};
use std::collections::BTreeMap;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
pub fn create_mcp_transport_spec(
    server: &str,
    config: &McpServerConfig,
    env: Option<&BTreeMap<String, String>>,
) -> Result<McpTransportSpec, McpError> {
    let failure = |kind, message| {
        let mut error = McpError::new(kind, message);
        error.phase = Some("create".into());
        error.server_name = Some(server.into());
        error
    };
    if config.transport == Some(Transport::Stdio) {
        let command = config
            .command
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                failure(
                    McpErrorKind::Connect,
                    format!("MCP server {server} stdio command is required"),
                )
            })?;
        let mut merged = env
            .map(crate::stdio_env::filter_host_env)
            .unwrap_or_default();
        merged.extend(config.env.clone().unwrap_or_default());
        return Ok(McpTransportSpec::Stdio {
            command: command.clone(),
            args: config.args.clone().unwrap_or_default(),
            cwd: config.cwd.clone(),
            env: merged,
        });
    }
    let url = config
        .url
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            failure(
                McpErrorKind::Connect,
                format!("MCP server {server} HTTP URL is required"),
            )
        })?;
    let url = url::Url::parse(url).map_err(|error| {
        failure(
            McpErrorKind::Connect,
            format!("MCP server {server} HTTP URL is invalid: {error}"),
        )
    })?;
    let mut headers = config.headers.clone().unwrap_or_default();
    if config.auth == Some(Auth::Mode(AuthMode::Bearer))
        || (config.auth.is_none() && config.bearer_token_env.is_some())
    {
        let name = config
            .bearer_token_env
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                failure(
                    McpErrorKind::Auth,
                    format!("MCP server {server} bearer auth requires bearerTokenEnv"),
                )
            })?;
        let token = env
            .and_then(|env| env.get(name))
            .cloned()
            .or_else(|| std::env::var(name).ok())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                failure(
                    McpErrorKind::Auth,
                    format!("MCP server {server} bearer token env {name} is not set"),
                )
            })?;
        headers.insert("authorization".into(), format!("Bearer {token}"));
    }
    Ok(McpTransportSpec::Http { url, headers })
}
pub struct McpTransportConnection {
    pub project_policy: bool,
    pub resolve_project_policy: Option<ProjectPolicyResolver>,
    pub server_name: String,
    pub spec: McpTransportSpec,
    pub connect_timeout: Duration,
    logger: Arc<Mutex<McpLogger>>,
    client: tokio::sync::OnceCell<Arc<McpClient>>,
    pub auth: Option<Arc<crate::auth::oauth_refresh::McpRefreshManager>>,
    pub auth_challenge_handler: Option<Arc<dyn crate::auth::challenge::McpAuthChallengeHandler>>,
    pub elicitation_ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
    server_requests: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown: tokio::sync::Mutex<bool>,
}
pub fn create_mcp_transport(
    server: &str,
    config: &McpServerConfig,
    env: Option<&BTreeMap<String, String>>,
    logger: Arc<Mutex<McpLogger>>,
) -> Result<McpTransportConnection, McpError> {
    let spec = create_mcp_transport_spec(server, config, env)?;
    let timeout = config.connect_timeout_ms.unwrap_or(15000.0);
    Ok(McpTransportConnection {
        project_policy: false,
        resolve_project_policy: None,
        server_name: server.into(),
        spec,
        connect_timeout: Duration::from_secs_f64(timeout.max(0.0) / 1000.0),
        logger,
        client: tokio::sync::OnceCell::new(),
        auth: None,
        auth_challenge_handler: None,
        elicitation_ui: None,
        server_requests: Mutex::new(None),
        shutdown: tokio::sync::Mutex::new(false),
    })
}
impl McpTransportConnection {
    pub async fn resolve_before_start(&self) -> Result<McpTransportSpec, McpError> {
        if let Some(resolve) = &self.resolve_project_policy {
            let current = resolve().await?;
            let mut spec =
                create_mcp_transport_spec(&self.server_name, &current.config, Some(&current.env))?;
            if let McpTransportSpec::Stdio { env, .. } = &mut spec {
                *env = crate::stdio_env::filter_host_env(&current.env);
                env.extend(current.config.env.unwrap_or_default());
            }
            Ok(spec)
        } else {
            Ok(self.spec.clone())
        }
    }
    pub async fn materialize(&self) -> Result<Arc<McpClient>, McpError> {
        self.client
            .get_or_try_init(|| async {
                let mut spec = self.resolve_before_start().await?;
                let shutdown = self.shutdown.lock().await;
                if *shutdown {
                    return Err(McpError::new(
                        McpErrorKind::Connect,
                        "MCP transport was shut down before spawn",
                    ));
                }
                if let (McpTransportSpec::Stdio { env, .. }, Some(auth)) = (&mut spec, &self.auth)
                    && let Some(tokens) = auth
                        .ensure_fresh()
                        .await
                        .map_err(|error| McpError::new(McpErrorKind::Auth, error.to_string()))?
                {
                    env.insert("OAUTH_ACCESS_TOKEN".into(), tokens.access_token);
                }
                let client = McpClient::materialize_with_policy(
                    &self.server_name,
                    &spec,
                    self.logger.clone(),
                    self.project_policy,
                )
                .await?;
                if let Some(auth) = &self.auth {
                    client.set_auth(auth.clone()).await;
                }
                if let Some(handler) = &self.auth_challenge_handler {
                    client.set_auth_challenge_handler(handler.clone()).await;
                }
                *self
                    .server_requests
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(client.install_elicitation(self.elicitation_ui.clone()));
                drop(shutdown);
                Ok::<_, McpError>(client)
            })
            .await
            .cloned()
    }
    pub fn client(&self) -> Result<Arc<McpClient>, McpError> {
        self.client.get().cloned().ok_or_else(|| {
            let mut error = McpError::new(
                McpErrorKind::Connect,
                format!("MCP server {} transport is not started", self.server_name),
            );
            error.phase = Some("create".into());
            error.server_name = Some(self.server_name.clone());
            error
        })
    }
    pub fn get_root_pid(&self) -> Option<u32> {
        self.client.get().and_then(|client| client.root_pid)
    }
}
pub async fn connect_mcp_transport(
    connection: &McpTransportConnection,
) -> Result<Arc<McpClient>, McpError> {
    let connect = async {
        let client = connection.materialize().await?;
        client.initialize(connection.connect_timeout).await?;
        Ok::<_, McpError>(client)
    };
    match tokio::time::timeout(connection.connect_timeout, connect).await {
        Ok(Ok(client)) => Ok(client),
        result => {
            let _ = shutdown_mcp_transport(connection).await;
            let mut error = match result {
                Ok(Err(error)) if error.kind == McpErrorKind::Auth => error,
                Ok(Err(error)) if error.kind != McpErrorKind::Timeout => McpError::new(
                    McpErrorKind::Connect,
                    format!(
                        "MCP server {} failed during connect: {error}",
                        connection.server_name
                    ),
                ),
                _ => McpError::new(
                    McpErrorKind::Timeout,
                    format!(
                        "MCP server {} timed out during connect after {}ms",
                        connection.server_name,
                        connection.connect_timeout.as_millis()
                    ),
                ),
            };
            error.phase = Some("connect".into());
            error.server_name = Some(connection.server_name.clone());
            error.retriable = error.kind != McpErrorKind::Auth;
            Err(error)
        }
    }
}
pub async fn shutdown_mcp_transport(connection: &McpTransportConnection) -> Result<(), McpError> {
    let mut shutdown = connection.shutdown.lock().await;
    if *shutdown {
        return Ok(());
    }
    if let Some(task) = connection
        .server_requests
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        task.abort();
    }
    if let Some(client) = connection.client.get() {
        let cleanup = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(pid) = client.root_pid {
                crate::process_tree::reap_process_tree(
                    pid,
                    Duration::from_millis(400),
                    Duration::from_millis(500),
                )
                .await;
            }
        };
        // Mirrors upstream: close runs concurrently, then is bounded to 400 ms after the reap (Promise.race).
        let mut close = Box::pin(client.close());
        tokio::pin!(cleanup);
        let mut closed = None;
        loop {
            tokio::select! {result=&mut close,if closed.is_none()=>{closed=Some(result);},()=&mut cleanup=>break}
        }
        let result = match closed {
            Some(result) => result,
            None => tokio::time::timeout(Duration::from_millis(400), close)
                .await
                .unwrap_or(Ok(())),
        };
        result?;
    } else {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    *shutdown = true;
    Ok(())
}
impl Drop for McpTransportConnection {
    fn drop(&mut self) {
        if let Some(task) = self
            .server_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}
