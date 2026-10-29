use super::{
    environment_grant_policy::EnvironmentGrantPolicy,
    policy_connection::connect_with_policy_signal,
    types::{
        Environment, ManagedClient, McpClient, PendingConnection, SharedState, SkillMcpClientInfo,
    },
};
use serde_json::Value;
use std::{sync::Arc, time::Instant};
use tokio::sync::watch;
pub async fn get_or_create_client(
    state: &SharedState,
    policy: &Arc<EnvironmentGrantPolicy>,
    base_key: String,
    info: &SkillMcpClientInfo,
    raw: &Value,
    explicit: &Environment,
    cdp_url: Option<&str>,
) -> super::Result<Arc<McpClient>> {
    let (shutdown, disconnect) = {
        let state = state.lock().await;
        if state.disposed {
            return Err("MCP manager has been shut down".into());
        }
        (
            state.shutdown_generation,
            *state
                .disconnected_sessions
                .get(&info.session_id)
                .unwrap_or(&0),
        )
    };
    let mut info = info.clone();
    let mut config = raw.clone();
    if let Some(revalidate) = &info.revalidate_declaration {
        let current = revalidate()
            .await?
            .ok_or("The MCP declaration is no longer available.")?;
        config = current.config;
        info.source_path = Some(current.source_path);
        info.source_kind = Some(current.source_kind);
    }
    let prepared = policy.prepare(&info, &config, explicit).await;
    let key = prepared
        .as_ref()
        .map(|p| format!("{base_key}::identity={}::policy={}", p.identity, p.digest))
        .unwrap_or(base_key);
    let token = uuid::Uuid::new_v4().to_string();
    let (sender, receiver) = watch::channel(None);
    let mut pending = {
        let mut state = state.lock().await;
        if state.disposed
            || state.shutdown_generation != shutdown
            || *state
                .disconnected_sessions
                .get(&info.session_id)
                .unwrap_or(&0)
                != disconnect
        {
            return Err("Session disconnected during MCP policy resolution.".into());
        }
        if let Some(existing) = state.clients.get_mut(&key) {
            existing.last_used_at = Instant::now();
            return Ok(existing.client.clone());
        }
        if let Some(pending) = state.pending_connections.get(&key) {
            Some(pending.result.clone())
        } else {
            state.pending_connections.insert(
                key.clone(),
                PendingConnection {
                    token: token.clone(),
                    result: receiver,
                },
            );
            None
        }
    };
    if let Some(receiver) = pending.as_mut() {
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| "MCP connection task ended".to_owned())?;
        }
    }
    if let Some(prepared) = &prepared {
        config = prepared.config.clone();
    } else {
        config=claude_code_compat_core::features::claude_code_mcp_loader::env_expander::expand_env_vars_in_object(&config,claude_code_compat_core::features::claude_code_mcp_loader::env_expander::ExpandEnvVarsOptions {trusted:!super::project_env_policy::uses_project_environment(&info)});
    }
    if let Some(cdp) = cdp_url {
        let mut args = config["args"].as_array().cloned().unwrap_or_default();
        args.push(Value::String("--cdp-endpoint".into()));
        args.push(Value::String(cdp.into()));
        config["args"] = Value::Array(args);
    }
    // Spawn independently of the caller: cancelling one waiter must not strand pending peers.
    let state = state.clone();
    let policy = policy.clone();
    let mut result_receiver = sender.subscribe();
    tokio::spawn(async move {
        let result = async {
            let materialize = async {
                if config["type"] == "stdio" {
                    super::stdio_client::create_stdio_client(
                        &info,
                        &config,
                        prepared.as_ref().map(|p| &p.environment),
                    )
                    .await
                } else if config["type"] == "http"
                    || config["type"] == "sse"
                    || config.get("url").is_some()
                {
                    super::http_client::create_http_client(&info, &config).await
                } else if config["command"].as_str().is_some_and(|command| !command.is_empty()) {
                    super::stdio_client::create_stdio_client(
                        &info,
                        &config,
                        prepared.as_ref().map(|p| &p.environment),
                    )
                    .await
                } else {
                    Err(format!("MCP server {:?} has no valid connection configuration: specify a URL or command in skill {:?}.", info.server_name, info.skill_name))
                }
            };
            let client = super::policy_connection::await_policy_signal(
                materialize,
                prepared.as_ref().map(|p| p.signal.clone()),
            )
            .await?;
            if config.get("oauth").is_some() {
                client
                    .set_auth_challenge_handler(Arc::new(SkillStepUp {
                        state: Arc::downgrade(&state),
                        policy: Arc::downgrade(&policy),
                        info: info.clone(),
                        config: std::sync::Mutex::new(config.clone()),
                        key: key.clone(),
                    }))
                    .await;
            }
            if let Err(error) =
                connect_with_policy_signal(&client, prepared.as_ref().map(|p| p.signal.clone()))
                    .await
            {
                if let Err(close_error) = client.close().await {
                    eprintln!("MCP failed connection cleanup: {close_error}");
                }
                return Err(error);
            }
            let policy_changed = if let Some(p) = &prepared {
                policy.version(&p.identity).await != p.version
            } else {
                false
            };
            let mut locked = state.lock().await;
            let stale = locked
                .pending_connections
                .get(&key)
                .is_none_or(|p| p.token != token)
                || locked.shutdown_generation != shutdown
                || *locked
                    .disconnected_sessions
                    .get(&info.session_id)
                    .unwrap_or(&0)
                    != disconnect
                || policy_changed;
            if stale {
                drop(locked);
                if let Err(error) = client.close().await {
                    eprintln!("MCP stale connection cleanup: {error}");
                }
                return Err("MCP connection superseded or policy/session changed.".into());
            }
            locked.clients.insert(
                key.clone(),
                ManagedClient {
                    client: client.clone(),
                    skill_name: info.skill_name,
                    last_used_at: Instant::now(),
                    identity: prepared.as_ref().map(|p| p.identity.clone()),
                },
            );
            Ok(client)
        }
        .await;
        let mut state = state.lock().await;
        if state
            .pending_connections
            .get(&key)
            .is_some_and(|p| p.token == token)
        {
            state.pending_connections.remove(&key);
        }
        sender.send_replace(Some(result));
    });
    loop {
        if let Some(result) = result_receiver.borrow().clone() {
            return result;
        }
        result_receiver
            .changed()
            .await
            .map_err(|_| "MCP connection task ended".to_owned())?;
    }
}
pub async fn get_or_create_client_with_retry_impl(
    state: &SharedState,
    policy: &Arc<EnvironmentGrantPolicy>,
    key: String,
    info: &SkillMcpClientInfo,
    config: &Value,
    env: &Environment,
    cdp: Option<&str>,
) -> super::Result<Arc<McpClient>> {
    match get_or_create_client(state, policy, key.clone(), info, config, env, cdp).await {
        Ok(client) => Ok(client),
        Err(error) => {
            if super::cleanup::force_reconnect(state, &key).await {
                get_or_create_client(state, policy, key, info, config, env, cdp).await
            } else {
                Err(error)
            }
        }
    }
}

struct SkillStepUp {
    state: std::sync::Weak<tokio::sync::Mutex<super::types::SkillMcpManagerState>>,
    policy: std::sync::Weak<EnvironmentGrantPolicy>,
    info: SkillMcpClientInfo,
    config: std::sync::Mutex<Value>,
    key: String,
}
impl maho_ext_mcp::auth::challenge::McpAuthChallengeHandler for SkillStepUp {
    fn reauthorize(&self, required: Vec<String>) -> futures::future::BoxFuture<'_, Result<(), maho_ext_mcp::errors::McpError>> {
        let config = {
            let mut config = self
                .config
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let scopes = config["oauth"]["scopes"]
                .as_array()
                .map(|s| {
                    s.iter()
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            config["oauth"]["scopes"] = serde_json::json!(
                super::oauth_compat::merge_scopes(&scopes, &required)
            );
            config.clone()
        };
        let info = self.info.clone();
        Box::pin(async move {
            let root = super::env_grants_store::resolve_grants_store_path(None)
                .parent()
                .expect("config directory")
                .to_path_buf();
            super::http_client::login(&info, &config, &root)
                .await
                .map_err(|e| {
                    maho_ext_mcp::errors::McpError::new(maho_ext_mcp::errors::McpErrorKind::Auth, e)
                })
        })
    }
    fn renew(&self) -> futures::future::BoxFuture<'_, Result<Arc<McpClient>, maho_ext_mcp::errors::McpError>> {
        let state = self.state.upgrade();
        let policy = self.policy.upgrade();
        let info = self.info.clone();
        let config = self
            .config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let key = self.key.clone();
        Box::pin(async move {
            let state = state.ok_or_else(|| {
                maho_ext_mcp::errors::McpError::new(
                    maho_ext_mcp::errors::McpErrorKind::Connect,
                    "MCP manager disposed",
                )
            })?;
            let policy = policy.ok_or_else(|| {
                maho_ext_mcp::errors::McpError::new(
                    maho_ext_mcp::errors::McpErrorKind::Connect,
                    "MCP policy disposed",
                )
            })?;
            super::cleanup::force_reconnect(&state, &key).await;
            let prepared = policy.prepare(&info, &config, &Environment::new()).await;
            let resolved = prepared.as_ref().map(|p| &p.config).unwrap_or(&config);
            let client = super::http_client::create_http_client(&info, resolved)
                .await
                .map_err(|e| {
                    maho_ext_mcp::errors::McpError::new(
                        maho_ext_mcp::errors::McpErrorKind::Connect,
                        e,
                    )
                })?;
            connect_with_policy_signal(&client, prepared.as_ref().map(|p| p.signal.clone()))
                .await
                .map_err(|e| {
                    maho_ext_mcp::errors::McpError::new(
                        maho_ext_mcp::errors::McpErrorKind::Connect,
                        e,
                    )
                })?;
            let client_key = prepared
                .as_ref()
                .map(|p| {
                    format!(
                        "{}::identity={}::policy={}",
                        super::manager::build_skill_mcp_client_key(&info, None),
                        p.identity,
                        p.digest
                    )
                })
                .unwrap_or_else(|| super::manager::build_skill_mcp_client_key(&info, None));
            state.lock().await.clients.insert(
                client_key,
                ManagedClient {
                    client: client.clone(),
                    skill_name: info.skill_name,
                    last_used_at: Instant::now(),
                    identity: prepared.map(|p| p.identity),
                },
            );
            Ok(client)
        })
    }
}
