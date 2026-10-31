use std::sync::{Arc, Mutex};
use crate::{catalog::McpCatalogCallback, catalog_cache::McpCachedServerCatalog, config_schema::ResolvedMcpConfig, connection::ServerConnection, log::McpLogger, service_types::McpConnectionEntry};

pub struct McpServiceRegistrationEntry {
    pub name: String,
    pub connection: Arc<ServerConnection>,
    pub logger: Arc<Mutex<McpLogger>>,
    pub agent_dir: Option<std::path::PathBuf>,
    pub cached_catalog: Option<McpCachedServerCatalog>,
    pub artifacts: Option<Arc<crate::guard::output_guard::McpOutputArtifacts>>,
    pub ensure_fresh: McpCatalogCallback,
    pub ensure_cached_tool_connected: McpCatalogCallback,
    /// service-register.ts `requestOrigin: () => options.requestOrigin?.(entry)`.
    pub request_origin: Option<crate::request_origin::McpRequestOriginFactory>,
    pub bind_presentation: Option<McpBindPresentation>,
    /// service-register.ts `createMcpInvocationResolver({owner: entry, ...})`: one resolver per
    /// server entry, shared by every catalog entry it offers.
    pub invocation: Option<Arc<crate::invocation::McpInvocationResolver>>,
}

pub async fn prepare_mcp_service_registration_entries(
    config: &ResolvedMcpConfig,
    entries: &[Arc<tokio::sync::Mutex<McpConnectionEntry>>],
) -> Vec<McpServiceRegistrationEntry> {
    prepare_mcp_service_registration_entries_with(config, entries, None, None).await
}
/// Presentation rebinding for a registration's UI entry point (upstream `bindPresentation`).
pub type McpBindPresentation = Arc<dyn Fn(&Arc<dyn maho_ext_api::ExtensionUi>) + Send + Sync>;
/// `binding`: the issuing session binding; registrations hold it weakly, so a released binding issues no origin.
pub async fn prepare_mcp_service_registration_entries_with(
    config: &ResolvedMcpConfig,
    entries: &[Arc<tokio::sync::Mutex<McpConnectionEntry>>],
    binding: Option<&Arc<crate::service::McpSessionBinding>>,
    tools: Option<crate::invocation::McpToolLookup>,
) -> Vec<McpServiceRegistrationEntry> {
    let binding = binding.map(Arc::downgrade);
    let mut prepared = Vec::with_capacity(entries.len());
    for entry in entries {
        let snapshot = entry.lock().await;
        let server_config = config.servers.get(&snapshot.name).and_then(|server| server.config.clone());
        let refresh_entry = entry.clone();
        let auth_entry = entry.clone();
        let origin = binding.clone().map(|binding| {
            let name = snapshot.name.clone(); let connection = snapshot.connection.clone();
            Arc::new(move || Some(binding.upgrade()?.request_origin(&name, &connection))) as crate::request_origin::McpRequestOriginFactory
        });
        let bind_presentation = binding.clone().map(|binding| {
            Arc::new(move |ui: &Arc<dyn maho_ext_api::ExtensionUi>| if let Some(binding) = binding.upgrade() {binding.bind_presentation(ui);}) as McpBindPresentation
        });
        let invocation = Some(Arc::new(invocation_resolver(config, entry, &snapshot, binding.clone(), origin.clone(), tools.clone())));
        prepared.push(McpServiceRegistrationEntry {
            request_origin: origin, bind_presentation, invocation,
            name: snapshot.name.clone(), connection: snapshot.connection.clone(), logger: snapshot.logger.clone(),
            agent_dir: snapshot.agent_dir.clone(), cached_catalog: snapshot.cached_catalog.clone(),
            artifacts: snapshot.artifacts.clone(),
            ensure_fresh: Arc::new(move || {
                let entry = auth_entry.clone();
                Box::pin(async move {
                    entry.lock().await.auth_plan.ensure_fresh().await
                })
            }),
            ensure_cached_tool_connected: Arc::new(move || {
                let entry = refresh_entry.clone(); let config = server_config.clone();
                Box::pin(async move {
                    if let Some(config) = config {crate::startup_race::connect_and_refresh_mcp_catalog(&mut *entry.lock().await, &config).await?;}
                    Ok(())
                })
            }),
        });
    }
    prepared
}

/// service-register.ts `createMcpInvocationResolver` for one connection entry.
///
/// DIVERGENCE: `contextRequired` stays false (an embedder that calls a tool body without a host
/// context keeps calling it); a call that does carry the host context is still fenced by the
/// operation and the permission authorizer. The binding's session-id check stands in for
/// senpi's `sessionManager` identity, and `publishCurrent` re-registration is left to the
/// existing tools-changed path.
fn invocation_resolver(
    config: &ResolvedMcpConfig,
    handle: &Arc<tokio::sync::Mutex<McpConnectionEntry>>,
    snapshot: &McpConnectionEntry,
    binding: Option<std::sync::Weak<crate::service::McpSessionBinding>>,
    request_origin: Option<crate::request_origin::McpRequestOriginFactory>,
    lookup: Option<crate::invocation::McpToolLookup>,
) -> crate::invocation::McpInvocationResolver {
    let name = snapshot.name.clone();
    let connection = snapshot.connection.clone();
    let server_config = config.servers.get(&name).and_then(|server| server.config.clone());
    let settings = config.settings.clone();
    let weak_entry = Arc::downgrade(handle);
    let is_current: Arc<dyn Fn() -> bool + Send + Sync> = {
        let (binding, name, connection) = (binding.clone(), name.clone(), connection.clone());
        Arc::new(move || match &binding {
            Some(binding) => binding.upgrade().is_some_and(|binding| binding.offers(&name, &connection)),
            None => true,
        })
    };
    let ready: McpCatalogCallback = {
        let (weak_entry, server_config, connection) = (weak_entry.clone(), server_config.clone(), connection.clone());
        Arc::new(move || {
            let (weak_entry, server_config, connection) = (weak_entry.clone(), server_config.clone(), connection.clone());
            Box::pin(async move {
                let Some(handle) = weak_entry.upgrade() else { return Ok(()) };
                let ensure_fresh: McpCatalogCallback = {
                    let handle = Arc::downgrade(&handle);
                    Arc::new(move || {
                        let handle = handle.clone();
                        Box::pin(async move {
                            let Some(handle) = handle.upgrade() else { return Ok(()) };
                            let plan = { let entry = handle.lock().await; crate::auth::context::ServerAuthPlan {mode: entry.auth_plan.mode, provider: entry.auth_plan.provider.clone(), refresh: entry.auth_plan.refresh.clone()} };
                            plan.ensure_fresh().await
                        })
                    })
                };
                crate::health::ensure_mcp_tool_call_connection(&connection, Some(&ensure_fresh)).await?;
                if let Some(server_config) = &server_config {
                    crate::startup_race::connect_and_refresh_mcp_catalog_unlocked(&handle, server_config).await?;
                }
                Ok(())
            })
        })
    };
    let catalog = Arc::new(CurrentLookup {server_config, settings, is_current: is_current.clone(), request_origin});
    let current: crate::invocation::McpInvocationCurrent = {
        let (weak_entry, catalog) = (weak_entry.clone(), catalog.clone());
        Arc::new(move |tool: String| {
            let (weak_entry, catalog) = (weak_entry.clone(), catalog.clone());
            Box::pin(async move {
                let handle = weak_entry.upgrade()?;
                let entry = handle.lock().await;
                catalog.find(&entry, &tool)
            })
        })
    };
    let current_now: crate::invocation::McpInvocationCurrentNow = Arc::new(move |tool: &str| {
        let handle = weak_entry.upgrade()?;
        // A refresh holding the entry is changing the catalog: not current until it settles.
        let entry = handle.try_lock().ok()?;
        catalog.find(&entry, tool)
    });
    crate::invocation::create_mcp_invocation_resolver(crate::invocation::McpInvocationScope {
        owner: snapshot.dispatch_owner.clone(),
        configuration: snapshot.config_hash.clone(),
        context_required: false,
        session_id: None,
        is_current,
        ready,
        current,
        current_now,
        lookup,
    })
}

/// service-register.ts `current(tool)`: the entry the server offers now, or none while its
/// metadata is not current.
struct CurrentLookup {
    server_config: Option<crate::config_schema::McpServerConfig>,
    settings: crate::config_schema::McpSettings,
    is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    request_origin: Option<crate::request_origin::McpRequestOriginFactory>,
}
impl CurrentLookup {
    fn find(&self, entry: &McpConnectionEntry, tool: &str) -> Option<crate::catalog::McpToolCatalogEntry> {
        let server_config = self.server_config.as_ref()?;
        if !(self.is_current)()
            || !entry.cache_refreshed_after_connect
            || entry.connection.state() != crate::connection::ServerConnectionState::Connected
        {
            return None;
        }
        let catalog = entry.cached_catalog.as_ref()?;
        let client = entry.connection.client().ok()?;
        let modern = catalog.modern || crate::modern::request::is_modern_mcp_client(&client);
        let timeout = std::time::Duration::from_secs_f64(server_config.request_timeout_ms.unwrap_or(30000.0) / 1000.0);
        let tools = catalog.tools.iter().filter(|tool| !modern || crate::modern::tool_schema::has_supported_modern_tool_schemas(tool)).cloned().collect::<Vec<_>>();
        let mut entries = crate::catalog::cached_tools_to_catalog_entries(&entry.name, &tools, client, timeout);
        for candidate in &mut entries {
            candidate.modern = modern;
            candidate.connection = Some(entry.connection.clone());
            candidate.agent_dir = entry.agent_dir.clone();
            candidate.artifacts = entry.artifacts.clone();
            candidate.output_guard = self.settings.output_guard.clone();
            candidate.request_origin = self.request_origin.clone();
        }
        crate::expose::policy::compute_mcp_exposure_policy(&entries, server_config, &self.settings)
            .filtered_entries
            .into_iter()
            .find(|candidate| candidate.tool == tool)
    }
}

pub async fn register_mcp_service_direct_tools(
    registrar: std::sync::Arc<dyn crate::tool_registrar::McpToolRegistrar>,
    config: &ResolvedMcpConfig,
    entries: &[std::sync::Arc<tokio::sync::Mutex<McpConnectionEntry>>],
    tool_search: Option<&mut maho_ext_tool_search::service::ToolSearchService>,
    registry: &mut crate::expose::tier_b::McpTierBRegistry,
    refresh_active_set_when_empty: bool,
    binding: Option<&Arc<crate::service::McpSessionBinding>>,
) -> Result<Option<crate::expose::session::McpSessionRegistration>, maho_ext_api::ExtensionFailure> {
    // senpi `ctx.tools`: the registrar's live definitions decide which operation a tool name dispatches.
    // Held weakly: the registered definitions carry this resolver, so a strong registrar would cycle.
    let tools: crate::invocation::McpToolLookup = {
        let registrar = Arc::downgrade(&registrar);
        Arc::new(move |name: &str| registrar.upgrade()?.live_tool_definition(name))
    };
    let prepared = prepare_mcp_service_registration_entries_with(config, entries, binding, Some(tools)).await;
    crate::expose::session::register_direct_mcp_tools(registrar, config, prepared, tool_search, registry, refresh_active_set_when_empty).await
}
