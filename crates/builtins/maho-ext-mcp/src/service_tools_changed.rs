//! Port of senpi mcp/service-tools-changed.ts (v2026.10.27-2).
//!
//! DIVERGENCE: upstream refreshes every live session binding sharing the connection (`targets()`); a native
//! `McpService` holds exactly one binding, so the target is the service's own registration, passed in as one
//! `McpToolsRefreshTarget`.
use crate::{
    config_schema::ResolvedMcpConfig,
    connection::{ServerConnectionState, ServerConnectionToolsChangedCause},
    errors::McpError,
    notifications::{McpListChangeCoalescer, diff_mcp_tool_names, format_mcp_list_changed_delta},
    service_types::McpConnectionEntry,
    wrap::McpAsyncErrorSink,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One session's registration of a server's catalog: a refresh re-registers it there when stale.
pub struct McpToolsRefreshTarget<'a> {
    pub registrar: Arc<dyn crate::tool_registrar::McpToolRegistrar>,
    pub registered_identity: Option<String>,
    pub register: Box<dyn FnOnce() -> BoxFuture<'a, Result<(), McpError>> + Send + 'a>,
}

/// Whether the entry still owns its catalog: upstream `isCurrent`, `credentialsCurrent`, the generation the
/// refresh started under and a connected state.
pub struct McpToolsRefreshScope {
    pub is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    pub credentials_current: Arc<dyn Fn() -> bool + Send + Sync>,
    pub on_credentials_changed: Option<Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>>,
    /// The startup connect that still owns registration registers its refreshed catalog itself.
    pub startup_owns_registration: bool,
    /// The shared catalog behind a pooled connection (`entry.connection instanceof SharedMcpLease`).
    pub shared: Option<Arc<crate::shared_lease::SharedMcpLease>>,
}

/// Coalesce a server's tools-changed signals into one refresh. `connect_only` is true when every merged
/// signal came from a connect rather than from a reported change. Returns the subscription task; the caller
/// keeps the coalescer to dispose it with the entry.
pub fn subscribe_mcp_tools_changed<F, Fut>(
    entry: &McpConnectionEntry,
    refresh: F,
    sink: McpAsyncErrorSink,
) -> (tokio::task::JoinHandle<()>, Arc<McpListChangeCoalescer>)
where
    F: Fn(bool) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), McpError>> + Send + 'static,
{
    let coalescer = Arc::new(McpListChangeCoalescer::new(None, None));
    let change_reported = Arc::new(AtomicBool::new(false));
    let refresh = Arc::new(refresh);
    let mut changes = entry.connection.on_tools_changed();
    let notifier = coalescer.clone();
    let task = tokio::spawn(async move {
        loop {
            match changes.recv().await {
                Ok(event) => {
                    if event.cause != ServerConnectionToolsChangedCause::Connect {
                        change_reported.store(true, Ordering::SeqCst);
                    }
                    let change_reported = change_reported.clone();
                    let refresh = refresh.clone();
                    notifier.notify_guarded(
                        move || async move {
                            let connect_only = !change_reported.swap(false, Ordering::SeqCst);
                            refresh(connect_only).await
                        },
                        sink.clone(),
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    (task, coalescer)
}

/// Re-list a server on a coalesced tools-changed signal and re-register: added tools enter INACTIVE, and
/// removed tools are tombstoned so a stale call fails cleanly. Every connect raises the signal too; a
/// connect-only refresh leaves an unchanged catalog registered, so the catalog lands once per session (#2177).
pub async fn refresh_mcp_tools_on_list_changed(
    entry: &Arc<tokio::sync::Mutex<McpConnectionEntry>>,
    targets: Vec<McpToolsRefreshTarget<'_>>,
    config: &ResolvedMcpConfig,
    connect_only: bool,
    scope: McpToolsRefreshScope,
) -> Result<(), McpError> {
    let (name, connection) = {
        let entry = entry.lock().await;
        (entry.name.clone(), entry.connection.clone())
    };
    let Some(server) = config
        .servers
        .get(&name)
        .and_then(|server| server.config.as_ref())
    else {
        return Ok(());
    };
    if connection.state() != ServerConnectionState::Connected {
        return Ok(());
    }
    if !(scope.is_current)() {
        return Ok(());
    }
    if !(scope.credentials_current)() {
        if let Some(changed) = scope.on_credentials_changed {
            changed().await;
        }
        return Ok(());
    }
    let generation = connection.generation();
    let owns_catalog = || {
        (scope.is_current)()
            && (scope.credentials_current)()
            && connection.generation() == generation
            && connection.state() == ServerConnectionState::Connected
    };
    if connect_only && scope.startup_owns_registration {
        return Ok(());
    }
    let registered_catalog = entry.lock().await.cached_catalog.clone();
    if let Some(shared) = &scope.shared {
        let refreshed = shared.catalog().await?;
        if !owns_catalog() {
            return Ok(());
        }
        entry.lock().await.cached_catalog = Some(refreshed);
    }
    let timeout =
        std::time::Duration::from_secs_f64(server.request_timeout_ms.unwrap_or(30000.0) / 1000.0);
    let catalog =
        crate::catalog::collect_tool_catalog(&name, connection.client()?, timeout).await?;
    if !owns_catalog() {
        return Ok(());
    }
    let mapped_names = |entries: &[crate::catalog::McpToolCatalogEntry]| {
        crate::expose::register::map_mcp_catalog_names(entries)
            .into_iter()
            .map(|named| named.name)
            .collect::<Vec<_>>()
    };
    let new_names = mapped_names(&catalog);
    // Before the first refresh, the registered names are the catalog the startup pass registered.
    let known = entry.lock().await.known_tool_names.clone();
    let known_names = match known {
        Some(known) => known,
        None => match &registered_catalog {
            None => new_names.clone(),
            Some(registered) => mapped_names(&crate::catalog::cached_tools_to_catalog_entries(
                &name,
                &registered.tools,
                connection.client()?,
                timeout,
            )),
        },
    };
    let diff = diff_mcp_tool_names(&known_names, &new_names);
    let cached = entry.lock().await.cached_catalog.clone();
    let identity = crate::catalog::mcp_registration_identity(&catalog, cached.as_ref());
    let stale = targets
        .into_iter()
        .filter(|target| {
            !connect_only || target.registered_identity.as_deref() != Some(identity.as_str())
        })
        .collect::<Vec<_>>();
    if !stale.is_empty() {
        // Registration reads the entry's cached catalog. A shared lease refreshed it above; nothing else
        // refreshes a non-shared connection's catalog after its startup connect (#2188).
        if scope.shared.is_none() {
            let (config_hash, agent_dir) = {
                let entry = entry.lock().await;
                (entry.config_hash.clone(), entry.agent_dir.clone())
            };
            let refreshed = crate::catalog_cache::collect_server_catalog_for_cache_in(
                &connection,
                timeout,
                &config_hash,
                agent_dir.as_deref(),
            )
            .await?;
            if !owns_catalog() {
                return Ok(());
            }
            entry.lock().await.cached_catalog = Some(refreshed.clone());
            if let Some(agent_dir) = &agent_dir
                && owns_catalog()
            {
                crate::catalog_cache::write_mcp_cached_server(agent_dir, &name, refreshed)
                    .map_err(|error| {
                        McpError::new(crate::errors::McpErrorKind::Connect, error.to_string())
                    })?;
            }
        }
        if !owns_catalog() {
            return Ok(());
        }
        // Tombstone removed tools BEFORE re-registration so the subsequent active-set write (which excludes
        // them) leaves the tombstones inactive.
        let total = stale.len();
        let mut failures = Vec::new();
        for target in stale {
            if !owns_catalog() {
                return Ok(());
            }
            let mut result = Ok(());
            for removed in &diff.removed {
                if let Err(error) = target.registrar.register_tool_with_renderers(
                    crate::notifications::build_mcp_tombstone_definition(removed, &name),
                    crate::notifications::mcp_tombstone_renderers(removed),
                ) {
                    result = Err(McpError::new(
                        crate::errors::McpErrorKind::ToolExec,
                        error.to_string(),
                    ));
                    break;
                }
            }
            if result.is_ok() {
                result = (target.register)().await;
            }
            if let Err(error) = result {
                failures.push(error);
            }
        }
        if !failures.is_empty() {
            let mut entry = entry.lock().await;
            entry.known_tool_names = Some(new_names);
            entry.last_list_changed_delta = Some(format_mcp_list_changed_delta(&diff));
            return Err(McpError::new(
                crate::errors::McpErrorKind::ToolExec,
                format!(
                    "MCP {name} tool refresh failed in {} of {total} session(s): {}",
                    failures.len(),
                    failures
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ));
        }
    }
    let mut entry = entry.lock().await;
    entry.known_tool_names = Some(new_names);
    entry.last_list_changed_delta = Some(format_mcp_list_changed_delta(&diff));
    Ok(())
}
