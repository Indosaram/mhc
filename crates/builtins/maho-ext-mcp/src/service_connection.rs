use crate::{
    config_schema::McpServerConfig,
    connection::ServerConnection,
    errors::McpError,
    host_registry::HostMcpRegistry,
    idle::McpConnectionLifecycle,
    log::McpLogger,
    reconnect::McpReconnect,
    service_types::{McpConnectionEntry, McpServerCounters},
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
pub struct McpSessionConnection {
    pub entry: Arc<tokio::sync::Mutex<McpConnectionEntry>>,
    pub lifecycle: Option<Arc<McpConnectionLifecycle>>,
    pub reconnect: Option<Arc<McpReconnect>>,
    pub shared: Option<Arc<crate::shared_lease::SharedMcpLease>>,
}
pub struct SessionConnectionOptions<'a> {
    pub registry: &'a HostMcpRegistry,
    pub owner: u64,
    pub key: &'a str,
    pub name: &'a str,
    pub config_hash: &'a str,
    pub config: McpServerConfig,
    /// The declaration the generation `config` was resolved from (service-connection.ts `declared`); literal
    /// secret warnings read it, never the resolved snapshot. `None` when the config declares no reference.
    pub declared: Option<&'a McpServerConfig>,
    pub agent_dir: &'a Path,
    pub env: Option<BTreeMap<String, String>>,
    /// The declaring session's UI: keys pooled connections by legacy-form policy (host-registry.ts).
    pub elicitation_ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
    /// Upstream `elicitationEnabled`: false (print/json sessions) withholds the elicitation capability.
    pub elicitation_enabled: bool,
}
/// host-registry.ts `attachShared`: an enforced private-form pool observes live readiness, so its key never
/// freezes a creation-time readiness; other pools key by whether legacy forms are advertised.
pub fn mcp_legacy_form_policy_key(
    ui: Option<&Arc<dyn maho_ext_api::ExtensionUi>>,
    elicitation_enabled: bool,
) -> String {
    let enforced = ui.and_then(crate::form_types::private_mcp_forms).is_some();
    if enforced {
        format!("private:{elicitation_enabled}")
    } else {
        format!(
            "{}:false",
            crate::elicitation::legacy_mcp_form_eligible(ui, elicitation_enabled)
        )
    }
}
#[derive(Debug, thiserror::Error)]
pub enum SessionConnectionError {
    #[error(transparent)]
    Logger(#[from] regex::Error),
    #[error(transparent)]
    Connection(#[from] McpError),
}
pub fn create_mcp_session_connection(
    options: SessionConnectionOptions<'_>,
) -> Result<McpSessionConnection, SessionConnectionError> {
    create_session_connection(options, None)
}
pub fn create_shared_mcp_session_connection(
    options: SessionConnectionOptions<'_>,
    cwd: &str,
) -> Result<McpSessionConnection, SessionConnectionError> {
    create_session_connection(options, Some(cwd))
}
fn create_session_connection(
    options: SessionConnectionOptions<'_>,
    shared_cwd: Option<&str>,
) -> Result<McpSessionConnection, SessionConnectionError> {
    let SessionConnectionOptions {
        registry,
        owner,
        key,
        name,
        config_hash,
        config,
        declared,
        agent_dir,
        env,
        elicitation_ui,
        elicitation_enabled,
    } = options;
    let logger = Arc::new(Mutex::new(McpLogger::new(name, agent_dir, None)?));
    let plan =
        crate::auth::context::resolve_server_auth_with(crate::auth::context::ServerAuthDeps {
            server_name: name,
            config: &config,
            agent_dir: Some(agent_dir),
            logger: Some(logger.clone()),
            redirect_url: None,
            on_redirect: None,
            client: reqwest::Client::new(),
        });
    if let Ok(warnings) =
        crate::auth::context::detect_literal_bearer_warnings(name, declared.unwrap_or(&config))
    {
        for warning in warnings {
            let _ = logger
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .log("warning", &warning, None, None);
        }
    }
    let refresh = plan.refresh.clone();
    let shared = shared_cwd
        .filter(|cwd| crate::sharing_policy::shareable(&config, Some(cwd)))
        .map(|_| {
            let pool_key = format!(
                "{key}\0legacy-form:{}",
                mcp_legacy_form_policy_key(elicitation_ui.as_ref(), elicitation_enabled)
            );
            registry.attach_shared(&pool_key, owner, || {
                let connection =
                    ServerConnection::new(name, config.clone(), env.clone(), logger.clone());
                if let Some(refresh) = refresh.clone() {
                    connection.set_auth(refresh);
                }
                crate::shared_connection::SharedMcpConnection::with_idle_timeout(
                    connection,
                    agent_dir.into(),
                    config_hash.into(),
                    std::time::Duration::from_secs_f64(
                        config.request_timeout_ms.unwrap_or(30000.0) / 1000.0,
                    ),
                    std::time::Duration::from_secs_f64(
                        config.idle_timeout_min.unwrap_or(10.0) * 60.0,
                    ),
                )
            })
        })
        .transpose()?;
    let connection = match &shared {
        Some(lease) => lease.base_connection(),
        None => registry.attach(
            key,
            owner,
            || ServerConnection::new(name, config.clone(), env, logger.clone()),
            false,
        ),
    };
    // The pool key carries elicitationEnabled, so every lease of a pooled connection agrees on it.
    connection.set_elicitation_enabled(elicitation_enabled);
    let policy = crate::transport::project_policy(config_hash);
    match &shared {
        // shared-connection.ts: readiness and projectPolicy.resolve aggregate over the pool's live leases.
        Some(lease) => lease.shared.set_owner_options(
            owner,
            crate::shared_connection::SharedLeaseOptions {
                elicitation_ui: elicitation_ui.clone(),
                elicitation_enabled,
                project_policy: policy,
            },
        ),
        None => {
            if let Some(policy) = policy {
                connection.set_project_policy(policy);
            }
        }
    }
    if plan.provider.is_some()
        && config.oauth.as_ref().and_then(|oauth| oauth.flow.as_ref())
            != Some(&crate::config_schema::OAuthFlow::ClientCredentials)
    {
        connection.set_auth_challenge_handler(Arc::new(
            crate::auth::challenge_login::InteractiveChallengeHandler::for_server(
                name,
                &config,
                agent_dir,
                Arc::downgrade(&connection),
            ),
        ));
    }
    if shared.is_none()
        && let Some(refresh) = refresh
    {
        connection.set_auth(refresh);
    }
    let entry = Arc::new(tokio::sync::Mutex::new(McpConnectionEntry {
        key: key.into(),
        name: name.into(),
        config_hash: config_hash.into(),
        connection: connection.clone(),
        logger,
        created_at_ms: chrono::Utc::now().timestamp_millis() as f64,
        counters: McpServerCounters::default(),
        agent_dir: Some(agent_dir.into()),
        cached_catalog: None,
        cache_refreshed_after_connect: false,
        auth_plan: plan,
        artifacts: None,
        known_tool_names: None,
        last_list_changed_delta: None,
        list_changed_tasks: Vec::new(),
        dispatch_owner: maho_ext_api::ToolDispatchOwner::new(),
        list_changed_coalescer: None,
    }));
    if let Some(authority) = crate::project_skill_policy::get_project_mcp_authority(&config) {
        let weak = Arc::downgrade(&connection);
        // host-registry.ts invalidateIdentity: a shared pool invalidates (dropping its catalog), not just bumps.
        let shared_pool = shared.as_ref().map(|lease| Arc::downgrade(&lease.shared));
        let path = authority.store_path.clone();
        let identity = authority.identity.clone();

        let (sender, mut changes) = tokio::sync::mpsc::unbounded_channel();
        let watcher = crate::env_grants_store::observe(
            path,
            Arc::new(move |observation| {
                let _ = sender.send(observation);
            }),
        );
        struct AbortWatcher(tokio::task::JoinHandle<()>);
        impl Drop for AbortWatcher {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let watcher = AbortWatcher(watcher);
        let task = tokio::spawn(async move {
            let _watcher = watcher;
            while let Some(observation) = changes.recv().await {
                let Some(connection) = weak.upgrade() else {
                    return;
                };
                if (observation.conservative
                    || crate::project_skill_policy::effective_grant_names(
                        &observation.snapshot,
                        &identity,
                    ) != authority.started_grant_names())
                    && let Err(error) =
                        match shared_pool.as_ref().and_then(std::sync::Weak::upgrade) {
                            Some(shared) => shared.invalidate().await,
                            None => connection.bump_generation().await,
                        }
                {
                    maho_ext_api::diagnostic(&format!("MCP grant invalidation failed: {error}"));
                }
            }
        });
        // Registration is synchronous; the entry is fresh and has no competing owner yet.
        entry
            .try_lock()
            .expect("fresh MCP connection entry")
            .list_changed_tasks
            .push(task);
    }
    if shared.is_some() {
        return Ok(McpSessionConnection {
            entry,
            lifecycle: None,
            reconnect: None,
            shared,
        });
    }
    let lifecycle = McpConnectionLifecycle::configure(connection.clone(), config.clone());
    let weak = Arc::downgrade(&entry);
    let reconnect = McpReconnect::configure(
        connection,
        Arc::new(move || {
            let weak = weak.clone();
            let config = config.clone();
            Box::pin(async move {
                let Some(entry) = weak.upgrade() else {
                    return Ok(());
                };
                let mut entry = entry.lock().await;
                entry.counters.reconnect_count += 1;
                entry.cache_refreshed_after_connect = false;
                if let Err(error) = entry.auth_plan.ensure_fresh().await {
                    // Disabled while the refresh ran: the failure is our own teardown, not a sign-in need.
                    if entry.connection.state()
                        == crate::connection::ServerConnectionState::Disabled
                    {
                        return Ok(());
                    }
                    return Err(crate::health::mark_mcp_connection_needs_auth(
                        &entry.connection,
                        &error,
                    )
                    .unwrap_or(error));
                }
                entry.connection.renew().await?;
                crate::startup_race::connect_and_refresh_mcp_catalog(&mut entry, &config).await?;
                if let Some(error) = entry.connection.last_error() {
                    return Err(error);
                }
                Ok::<_, McpError>(())
            })
        }),
        Arc::new(|| true),
        Arc::new(|| {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes).expect("OS entropy unavailable for MCP reconnect jitter");
            (u64::from_le_bytes(bytes) >> 11) as f64 / ((1u64 << 53) as f64)
        }),
    );
    Ok(McpSessionConnection {
        entry,
        lifecycle: Some(lifecycle),
        reconnect: Some(reconnect),
        shared: None,
    })
}
pub async fn dispose_entry_connection(
    entry: &McpSessionConnection,
    registry: &HostMcpRegistry,
    owner: u64,
) -> Result<(), crate::host_registry::RegistryDetachError> {
    {
        let mut entry = entry.entry.lock().await;
        for task in entry.list_changed_tasks.drain(..) {
            task.abort();
        }
        if let Some(coalescer) = entry.list_changed_coalescer.take() {
            coalescer.dispose();
        }
    }
    if let Some(lease) = &entry.shared {
        lease.dispose();
        return Ok(());
    }
    if let Some(reconnect) = &entry.reconnect {
        reconnect.dispose();
    }
    if let Some(lifecycle) = &entry.lifecycle {
        lifecycle.dispose();
    }
    let key = entry.entry.lock().await.key.clone();
    registry.detach(&key, owner).await
}
