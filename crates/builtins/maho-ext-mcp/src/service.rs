use crate::{
    config::{McpConfigValidationError, load_mcp_config},
    config_schema::{LoadMcpConfigOptions, McpServerState, ResolvedMcpConfig},
    host_registry::HostMcpRegistry,
    service_connection::{
        McpSessionConnection, SessionConnectionOptions, create_shared_mcp_session_connection,
        dispose_entry_connection,
    },
    service_types::McpServerSnapshot,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
/// Upstream `getAuthTarget` result.
pub struct McpAuthTarget {
    pub config: crate::config_schema::McpServerConfig,
    pub agent_dir: PathBuf,
    pub callback_url: Option<String>,
}
/// A failed `/mcp test`, with the elapsed time the failure message reports.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct McpTestFailure {
    pub elapsed_ms: f64,
    pub error: McpServiceError,
}
#[derive(Debug, thiserror::Error)]
pub enum McpServiceError {
    #[error(transparent)]
    Config(#[from] McpConfigValidationError),
    #[error(transparent)]
    Logger(#[from] regex::Error),
    #[error(transparent)]
    SessionConnection(#[from] crate::service_connection::SessionConnectionError),
    #[error(transparent)]
    Detach(#[from] crate::host_registry::RegistryDetachError),
    #[error(transparent)]
    Connection(#[from] crate::errors::McpError),
    #[error(transparent)]
    OAuth(#[from] crate::auth::oauth::OAuthRequestError),
    #[error(transparent)]
    Artifact(#[from] std::io::Error),
    #[error(transparent)]
    Extension(#[from] maho_ext_api::ExtensionFailure),
}
/// Native equivalent of upstream's MCP-owned `nativeToolSearch` gate. The resolved
/// `settings.nativeToolSearch` lives behind the service's async mutex, but the shared
/// tool-search adapter asks for it synchronously (`isMcpNativeToolSearchEnabled`), so
/// every resolved config publishes the setting into this shared atomic. The host takes
/// the handle off the service and binds it into the tool-search adapter it owns
/// (upstream `installMcpNativeToolSearchGate`).
#[derive(Clone, Debug, Default)]
pub struct McpNativeToolSearchGate {
    state: Arc<std::sync::atomic::AtomicU8>,
}
impl McpNativeToolSearchGate {
    const UNSET: u8 = 0;
    const DISABLED: u8 = 1;
    const AUTO: u8 = 2;
    const ENABLED: u8 = 3;
    /// Publish the resolved `settings.nativeToolSearch` (`auto | true | false | undefined`).
    pub fn publish(&self, setting: Option<&crate::config_schema::NativeToolSearch>) {
        use crate::config_schema::NativeToolSearch;
        let state = match setting {
            None => Self::UNSET,
            Some(NativeToolSearch::Enabled(false)) => Self::DISABLED,
            Some(NativeToolSearch::Enabled(true)) => Self::ENABLED,
            Some(NativeToolSearch::Auto(_)) => Self::AUTO,
        };
        self.state.store(state, std::sync::atomic::Ordering::SeqCst);
    }
    /// Upstream `isMcpNativeToolSearchEnabled`: `setting === true || setting === "auto"`.
    pub fn enabled(&self) -> bool {
        matches!(
            self.state.load(std::sync::atomic::Ordering::SeqCst),
            Self::AUTO | Self::ENABLED
        )
    }
}
/// Registration seam bound by the host so `attach_session` (upstream
/// `attachSession`), skill attach and the deferred startup-race connects
/// (upstream `raceMcpStartupConnect`) all register tools through the same
/// registrar and shared tool-search service.
#[derive(Clone)]
pub struct McpRegistrationInputs {
    pub registrar: Arc<dyn crate::tool_registrar::McpToolRegistrar>,
    pub tool_search:
        Option<Arc<tokio::sync::Mutex<maho_ext_tool_search::service::ToolSearchService>>>,
}
/// `startup_settled` marks the one announcement that closes a session's startup window (senpi#4828).
pub type McpWireStatusListener =
    Arc<dyn Fn(Option<&str>, &crate::service_types::McpWireStatusSnapshot, bool) + Send + Sync>;

pub struct McpAttachOptions<'a> {
    pub cwd: &'a Path,
    pub agent_dir: &'a Path,
    pub env: &'a BTreeMap<String, String>,
    pub project_trusted: bool,
    pub declarations: &'a [maho_ext_api::RegisteredMcpServerDeclaration],
    pub mode: Option<maho_ext_api::ExtensionMode>,
    pub session_id: Option<&'a str>,
    pub ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
    pub has_ui: bool,
    pub extension_connects_mcp_servers: bool,
}

pub struct McpService {
    pub registry: Arc<HostMcpRegistry>,
    pub config: Option<ResolvedMcpConfig>,
    pub connections: BTreeMap<String, McpSessionConnection>,
    owner: u64,
    agent_dir: Option<PathBuf>,
    session_cwd: Option<PathBuf>,
    session_env: BTreeMap<String, String>,
    project_trusted: bool,
    session_mode: Option<maho_ext_api::ExtensionMode>,
    /// Skill server warnings already reported (upstream `#skillServerWarnings`): each is shown once.
    skill_server_warnings: std::collections::BTreeSet<String>,
    deferred: Arc<crate::startup_race::McpDeferredAttach>,
    deferred_registration: Arc<crate::startup_race::McpDeferredAttach>,
    pending_auth:
        Arc<std::sync::Mutex<BTreeMap<String, crate::auth::oauth_provider::McpOAuthProvider>>>,
    interactive_auth_servers: Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
    policy_notices: Arc<crate::policy_notices::PolicyNotices>,
    policy_notice_event: Option<Arc<dyn Fn(serde_json::Value) + Send + Sync>>,
    elicitation_ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
    /// service.ts: `declarer.context.mode !== "print" && declarer.context.mode !== "json"`.
    elicitation_enabled: bool,
    output_artifacts: Arc<crate::guard::output_guard::McpOutputArtifacts>,
    registration: Option<crate::expose::session::McpSessionRegistration>,
    tier_b_registry: crate::expose::tier_b::McpTierBRegistry,
    native_tool_search_gate: McpNativeToolSearchGate,
    self_weak: Option<std::sync::Weak<tokio::sync::Mutex<McpService>>>,
    registration_inputs: Option<McpRegistrationInputs>,
    /// Servers whose configuration references failed to resolve in the last sync (config-generation.ts).
    pub reference_errors: BTreeMap<String, maho_ai::utils::config_reference::ConfigReferenceError>,
    /// Upstream `McpSessionBinding`: shared with registrations so an origin is issued without the service lock.
    binding: Arc<McpSessionBinding>,
    /// Upstream `#registrationListeners`: bumped after every completed catalog registration.
    registration_events: tokio::sync::watch::Sender<u64>,
    wire_status_listeners: Arc<std::sync::Mutex<Vec<(u64, McpWireStatusListener)>>>,
    next_wire_status_listener_id: Arc<std::sync::atomic::AtomicU64>,
    wire_status_by_session: BTreeMap<String, crate::service_types::McpWireStatusSnapshot>,
    latest_wire_status: crate::service_types::McpWireStatusSnapshot,
    startup_windows: Arc<crate::startup_window::McpStartupWindows>,
}
/// Upstream `McpSessionBinding`: lifetime, presentation and declared sources of one session's binding.
pub struct McpSessionBinding {
    /// Aborted when the binding is replaced or released, ending every request origin it issued.
    lifetime: std::sync::Mutex<maho_ext_api::AbortSignal>,
    /// Rebound on session start and every input.
    presentation: std::sync::Mutex<Option<maho_core::mcp_mrtr_presentations::McpMrtrPresentation>>,
    sources: std::sync::Mutex<BTreeMap<String, crate::config_schema::McpServerSource>>,
    /// Pooled servers: this owner's lease, whose own request lifetime also bounds the origin.
    leases:
        std::sync::Mutex<BTreeMap<String, std::sync::Weak<crate::shared_lease::SharedMcpLease>>>,
    /// The connection each server the binding's config declares maps to, with its credential check
    /// (upstream `declares(binding.config, entry.name, entry.configHash)` and `entry.credentialsCurrent`).
    declared: std::sync::Mutex<BTreeMap<String, McpDeclaredServer>>,
    /// The id of the session this binding serves (upstream `binding.context.sessionManager`), once a
    /// session context bound it.
    session_id: std::sync::Mutex<Option<String>>,
    /// Further execution scopes of one runtime this binding also serves (brain/hand S16c).
    scopes: std::sync::Mutex<BTreeMap<String, crate::service_scopes::McpBindingScope>>,
    /// Its own context's elicitation UI lookup (returns `None` when `has_ui` is false).
    context_ui: std::sync::Mutex<crate::request_origin::McpElicitationUiLookup>,
}
impl Default for McpSessionBinding {
    fn default() -> Self {
        Self {
            lifetime: Default::default(),
            presentation: Default::default(),
            sources: Default::default(),
            leases: Default::default(),
            declared: Default::default(),
            session_id: Default::default(),
            scopes: Default::default(),
            context_ui: std::sync::Mutex::new(Arc::new(|| None)),
        }
    }
}
/// One declared server as of the binding's last sync.
struct McpDeclaredServer {
    connection: std::sync::Weak<crate::connection::ServerConnection>,
    credentials_current: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl McpSessionBinding {
    /// Record the session this binding serves; its MCP calls must come from that session.
    pub fn bind_session(&self, session_id: &str) {
        *self
            .session_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(session_id.to_owned());
    }
    pub fn session_id(&self) -> Option<String> {
        self.session_id
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn bind_presentation(&self, ui: &Arc<dyn maho_ext_api::ExtensionUi>) {
        self.bind_presentation_for(ui, None);
    }
    pub fn bind_presentation_for(
        &self,
        ui: &Arc<dyn maho_ext_api::ExtensionUi>,
        session_id: Option<&str>,
    ) {
        if let Some(presentation) = maho_core::mcp_mrtr_presentations::get_mcp_mrtr_presentation(ui)
        {
            crate::service_scopes::bind_scope_presentation(
                &self.presentation,
                &self.scopes,
                presentation,
                session_id,
            );
        }
    }
    pub fn bind_context_ui(&self, ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>, has_ui: bool) {
        *self
            .context_ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            crate::service_scopes::elicitation_ui_of(ui, has_ui);
    }
    pub fn join_scope(
        &self,
        session_id: &str,
        ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
        has_ui: bool,
    ) -> bool {
        let own = self.session_id();
        crate::service_scopes::join_binding_scope(
            own.as_deref(),
            &self.scopes,
            session_id,
            ui,
            has_ui,
        )
    }
    pub fn serves_scope(&self, session_id: Option<&str>) -> bool {
        crate::service_scopes::binding_serves_scope(&self.scopes, session_id)
    }
    fn lifetime(&self) -> maho_ext_api::AbortSignal {
        self.lifetime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn renew(&self) {
        let mut lifetime = self
            .lifetime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lifetime.abort();
        *lifetime = maho_ext_api::AbortSignal::default();
        drop(lifetime);
        // Upstream gives a renewed binding a fresh presentation from `ctx.ui`; the previous session's is never kept.
        *self
            .presentation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        self.scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
    fn release(&self) {
        self.lifetime().abort();
    }
    /// The binding's config still declares `name` on exactly this connection (upstream `entry.isCurrent`).
    fn declares_connection(
        &self,
        name: &str,
        connection: &Arc<crate::connection::ServerConnection>,
    ) -> bool {
        self.declared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .and_then(|server| server.connection.upgrade())
            .is_some_and(|current| Arc::ptr_eq(&current, connection))
    }
    /// The credential check the binding's last sync admitted `name` under (upstream `entry.credentialsCurrent`).
    fn credentials_current(&self, name: &str) -> Arc<dyn Fn() -> bool + Send + Sync> {
        self.declared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .map_or_else(
                || Arc::new(|| true) as Arc<dyn Fn() -> bool + Send + Sync>,
                |server| server.credentials_current.clone(),
            )
    }
    /// invocation.ts `scope.isCurrent`: the binding is live, still declares `name` on exactly this
    /// connection, and the credentials it was admitted under are current.
    pub(crate) fn offers(
        &self,
        name: &str,
        connection: &Arc<crate::connection::ServerConnection>,
    ) -> bool {
        !self.lifetime().is_aborted() && self.declares(name, connection)
    }
    /// The binding's config still declares `name` on exactly this connection, and its credentials are the ones
    /// the connection was keyed by (upstream request-origin `isCurrent`).
    fn declares(&self, name: &str, connection: &Arc<crate::connection::ServerConnection>) -> bool {
        let credentials_current = {
            let declared = self
                .declared
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match declared.get(name) {
                Some(server)
                    if server
                        .connection
                        .upgrade()
                        .is_some_and(|current| Arc::ptr_eq(&current, connection)) =>
                {
                    server.credentials_current.clone()
                }
                _ => return false,
            }
        };
        // Read outside the lock: the credential check reads the token store.
        credentials_current()
    }
    /// Upstream `#requestOrigin(binding, entry)`: the binding-issued owner of every MRTR/private-form request
    /// made on this session's behalf.
    pub fn request_origin(
        self: &Arc<Self>,
        name: &str,
        connection: &Arc<crate::connection::ServerConnection>,
    ) -> Arc<crate::request_origin::McpRequestOrigin> {
        self.request_origin_for(name, connection, None)
    }
    pub fn request_origin_for(
        self: &Arc<Self>,
        name: &str,
        connection: &Arc<crate::connection::ServerConnection>,
        caller_session_id: Option<&str>,
    ) -> Arc<crate::request_origin::McpRequestOrigin> {
        let binding_session_id = self.session_id();
        let binding_ui = self
            .context_ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let caller_scope = crate::service_scopes::caller_scope(
            binding_session_id.as_deref(),
            &self.presentation,
            &binding_ui,
            &self.scopes,
            caller_session_id,
        );
        let (presentation, scope_id, scope_ui) = match caller_scope {
            Some(scope) => (scope.presentation, Some(scope.session_id), Some(scope.ui)),
            None => (
                self.presentation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
                None,
                None,
            ),
        };
        let private_forms = presentation
            .as_ref()
            .and_then(|presentation| presentation.private_forms.clone());
        let source = self
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .copied();
        let server_source = Some(match source {
            Some(
                crate::config_schema::McpServerSource::Project
                | crate::config_schema::McpServerSource::Claude,
            ) => crate::form_types::McpFormServerSource::Project,
            Some(
                crate::config_schema::McpServerSource::Skill
                | crate::config_schema::McpServerSource::Extension,
            ) => crate::form_types::McpFormServerSource::Plugin,
            _ => crate::form_types::McpFormServerSource::User,
        });
        let lifetime = self.lifetime();
        let mut sources = vec![lifetime.clone(), connection.request_lifetime_signal()];
        sources.extend(
            self.leases
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(name)
                .and_then(std::sync::Weak::upgrade)
                .map(|lease| lease.request_lifetime_signal()),
        );
        let linked = crate::wrap::McpLinkedSignal::new(sources);
        let signal = linked.signal.clone();
        let generation = connection.generation();
        let current_connection = Arc::downgrade(connection);
        let authority_connection = Arc::downgrade(connection);
        let binding = Arc::downgrade(self);
        let declared_name = name.to_owned();
        crate::request_origin::create_mcp_request_origin(
            crate::request_origin::McpRequestOrigin::new(
                Arc::as_ptr(self) as usize,
                connection.clone(),
                signal,
                presentation,
                private_forms.as_ref().and_then(|forms| (forms.capture)()),
                private_forms.is_some(),
                name.into(),
                server_source,
                Arc::new(move || {
                    // The linked signal lives as long as this check: dropping the origin ends its watcher.
                    let _keep = &linked;
                    let Some(connection) = current_connection.upgrade() else {
                        return false;
                    };
                    !lifetime.is_aborted()
                        && connection.generation() == generation
                        && binding
                            .upgrade()
                            .is_some_and(|binding| binding.declares(&declared_name, &connection))
                }),
                Arc::new(
                    move |_signal| -> futures::future::BoxFuture<
                        'static,
                        Result<serde_json::Value, crate::errors::McpError>,
                    > {
                        let connection = authority_connection.upgrade();
                        Box::pin(async move {
                            let changed = || -> crate::errors::McpError {
                                crate::modern::errors::McpMrtrError::new(
                                    crate::modern::errors::McpMrtrErrorCode::ContextChanged,
                                )
                                .into()
                            };
                            let Some(connection) = connection else {
                                return Err(changed());
                            };
                            if let Some(renewal) = connection.generation_renewal()
                                && renewal.changed()?
                            {
                                return Err(changed());
                            }
                            // Same observed-revocation boundary as ordinary MCP requests; never prompts for a new grant.
                            match connection.project_policy() {
                                Some(resolve) => {
                                    let materialized = resolve().await?;
                                    Ok(
                                        serde_json::json!({"config": materialized.config, "env": materialized.env}),
                                    )
                                }
                                None => Ok(serde_json::Value::Null),
                            }
                        })
                    },
                ),
            )
            .with_scope(scope_id, scope_ui),
        )
    }
}
impl McpService {
    pub fn new(registry: Arc<HostMcpRegistry>, owner: u64) -> Self {
        Self {
            registry,
            config: None,
            connections: BTreeMap::new(),
            owner,
            agent_dir: None,
            session_cwd: None,
            session_env: BTreeMap::new(),
            project_trusted: false,
            session_mode: None,
            skill_server_warnings: Default::default(),
            deferred: Default::default(),
            deferred_registration: Default::default(),
            pending_auth: Arc::default(),
            interactive_auth_servers: Arc::default(),
            policy_notices: Default::default(),
            policy_notice_event: None,
            elicitation_ui: None,
            elicitation_enabled: true,
            output_artifacts: Arc::new(Default::default()),
            registration: None,
            tier_b_registry: Default::default(),
            native_tool_search_gate: McpNativeToolSearchGate::default(),
            self_weak: None,
            registration_inputs: None,
            reference_errors: BTreeMap::new(),
            binding: Arc::default(),
            registration_events: tokio::sync::watch::Sender::new(0),
            wire_status_listeners: Arc::default(),
            next_wire_status_listener_id: Arc::default(),
            wire_status_by_session: BTreeMap::new(),
            latest_wire_status: Default::default(),
            startup_windows: Arc::default(),
        }
    }
    /// Upstream `bindMcpMrtrPresentation`: keeps the binding's presentation current (a missing one keeps
    /// the last grant, as upstream does).
    pub fn bind_mcp_mrtr_presentation(&self, ui: &Arc<dyn maho_ext_api::ExtensionUi>) {
        self.binding.bind_presentation(ui);
    }
    pub fn bind_mcp_mrtr_presentation_for(
        &self,
        ui: &Arc<dyn maho_ext_api::ExtensionUi>,
        session_id: Option<&str>,
    ) {
        self.binding.bind_presentation_for(ui, session_id);
    }
    /// The binding handle, for hooks that rebind presentation without taking the service lock.
    pub fn session_binding(&self) -> Arc<McpSessionBinding> {
        self.binding.clone()
    }
    pub fn request_origin(
        &self,
        name: &str,
        connection: &Arc<crate::connection::ServerConnection>,
    ) -> Arc<crate::request_origin::McpRequestOrigin> {
        self.binding.request_origin(name, connection)
    }
    pub fn set_policy_notice_event(&mut self, emit: Arc<dyn Fn(serde_json::Value) + Send + Sync>) {
        self.policy_notice_event = Some(emit);
    }
    pub fn set_elicitation_ui(&mut self, ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>) {
        self.elicitation_ui = ui;
    }
    /// Upstream `elicitationEnabled` from the declaring session's mode: print and json sessions withhold the
    /// elicitation capability entirely.
    pub fn set_session_mode(&mut self, mode: maho_ext_api::ExtensionMode) {
        self.session_mode = Some(mode);
        self.elicitation_enabled = !matches!(
            mode,
            maho_ext_api::ExtensionMode::Print | maho_ext_api::ExtensionMode::Json
        );
    }
    /// Subscribe to live MCP inventory transitions for session-scoped hosts.
    pub fn on_wire_status_changed(
        &mut self,
        listener: McpWireStatusListener,
    ) -> Box<dyn FnOnce() + Send> {
        let id = self
            .next_wire_status_listener_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.wire_status_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, listener));
        let listeners = self.wire_status_listeners.clone();
        Box::new(move || {
            listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|(entry_id, _)| *entry_id != id);
        })
    }
    /// Return the attach-time inventory captured for one session.
    pub fn get_wire_status_snapshot(
        &self,
        session_id: Option<&str>,
    ) -> crate::service_types::McpWireStatusSnapshot {
        if session_id.is_none()
            && !self
                .binding
                .scopes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        {
            return Default::default();
        }
        match session_id {
            None => self.latest_wire_status.clone(),
            Some(id) => self
                .wire_status_by_session
                .get(id)
                .cloned()
                .unwrap_or_default(),
        }
    }
    /// Refresh and return the current session-owned MCP wire inventory.
    pub async fn refresh_wire_status_snapshot(
        &mut self,
        session_id: Option<&str>,
    ) -> crate::service_types::McpWireStatusSnapshot {
        if session_id.is_none()
            && !self
                .binding
                .scopes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        {
            return Default::default();
        }
        self.capture_wire_status(session_id, None).await
    }
    /// Close a startup window release on the service.
    pub async fn release_startup_window(
        &mut self,
        session_id: &str,
        release: crate::startup_window::StartupWindowRelease,
    ) {
        let _ = self
            .capture_wire_status(Some(session_id), Some(release))
            .await;
    }
    async fn capture_wire_status(
        &mut self,
        session_id: Option<&str>,
        release: Option<crate::startup_window::StartupWindowRelease>,
    ) -> crate::service_types::McpWireStatusSnapshot {
        let snapshot = self.wire_status_snapshot().await;
        let previous = self.get_wire_status_snapshot(session_id);
        self.latest_wire_status = snapshot.clone();
        if let Some(session_id) = session_id {
            self.wire_status_by_session
                .insert(session_id.to_owned(), snapshot.clone());
        }
        if release.is_none() && previous == snapshot {
            return snapshot;
        }
        if release.is_none() && self.startup_windows.hold(session_id) {
            return snapshot;
        }
        let startup_settled =
            release == Some(crate::startup_window::StartupWindowRelease::StartupSettled);
        let listeners: Vec<McpWireStatusListener> = self
            .wire_status_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in listeners {
            listener(session_id, &snapshot, startup_settled);
        }
        snapshot
    }
    pub fn tier_b_searchable(&self) -> Vec<crate::skills::RegisteredSkillTool> {
        self.registration
            .as_ref()
            .map(|reg| {
                reg.registration
                    .searchable
                    .iter()
                    .map(|tool| crate::skills::RegisteredSkillTool {
                        name: tool.name.clone(),
                        tool_name: tool.tool_name.clone(),
                        server: tool.server.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn activate_skill_mcp_tools(&self, targets: &[String]) {
        if let Some(reg) = &self.registration {
            let _ = (reg.registration.activate)(targets);
        }
    }
    /// A further execution scope of the runtime joins its binding (brain/hand S16c) without a rebind,
    /// a config re-sync or an abort; only its own wire status is published.
    pub async fn attach_scope(
        &mut self,
        session_id: &str,
        ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
        has_ui: bool,
    ) {
        if self.binding.join_scope(session_id, ui, has_ui)
            && self.session_mode == Some(maho_ext_api::ExtensionMode::Rpc)
        {
            let _ = self.refresh_wire_status_snapshot(Some(session_id)).await;
        }
    }
    /// Bind the service's own handle so a deferred startup-race connect can
    /// re-register tools after the attach deadline without holding the session lock.
    pub fn bind_self(&mut self, weak: std::sync::Weak<tokio::sync::Mutex<McpService>>) {
        self.self_weak = Some(weak);
    }
    /// Bind the registrar + shared tool-search service used by every registration
    /// trigger (`attach_session`, skill attach, deferred startup connects).
    pub fn bind_registration(
        &mut self,
        registrar: Arc<dyn crate::tool_registrar::McpToolRegistrar>,
        tool_search: Option<
            Arc<tokio::sync::Mutex<maho_ext_tool_search::service::ToolSearchService>>,
        >,
    ) {
        self.registration_inputs = Some(McpRegistrationInputs {
            registrar,
            tool_search,
        });
    }
    pub async fn attach_session(
        &mut self,
        cwd: &Path,
        agent_dir: &Path,
        env: &BTreeMap<String, String>,
        project_trusted: bool,
        declarations: &[maho_ext_api::RegisteredMcpServerDeclaration],
    ) -> Result<(), McpServiceError> {
        let session_id = self.binding.session_id();
        self.attach_session_with_options(McpAttachOptions {
            cwd,
            agent_dir,
            env,
            project_trusted,
            declarations,
            mode: self.session_mode,
            session_id: session_id.as_deref(),
            ui: self.elicitation_ui.clone(),
            has_ui: self.elicitation_ui.is_some(),
            extension_connects_mcp_servers: false,
        })
        .await
    }
    pub async fn attach_session_with_options(
        &mut self,
        options: McpAttachOptions<'_>,
    ) -> Result<(), McpServiceError> {
        let mode = options.mode.or(self.session_mode);
        let session_id = options.session_id.map(str::to_owned);
        let window = if mode == Some(maho_ext_api::ExtensionMode::Rpc) {
            session_id
                .as_deref()
                .map(|id| (id.to_owned(), self.startup_windows.open(id)))
        } else {
            None
        };
        let mut startup_connects: Vec<futures::future::BoxFuture<'static, ()>> = Vec::new();
        let result = async {
            // A new attach replaces the binding: origins the previous one issued end here.
            self.binding.renew();
            if let Some(id) = &session_id {
                self.binding.bind_session(id);
            }
            self.binding
                .bind_context_ui(options.ui.clone(), options.has_ui);
            self.project_trusted = options.project_trusted;
            let mut config = load_mcp_config(LoadMcpConfigOptions {
                cwd: options.cwd,
                agent_dir: options.agent_dir,
                env: options.env,
                project_trusted: options.project_trusted,
                home_dir: None,
                claude_config_dir: None,
            })?;
            // An extension handling mcp_servers_change connects the registered servers; connecting them here too
            // would open a second connection per server (senpi#4597).
            if !options.extension_connects_mcp_servers {
                crate::config::merge_extension_mcp_servers_in(
                    &mut config,
                    options.declarations,
                    Some(options.agent_dir),
                )?;
            }
            if let Some(previous) = &self.config {
                for (name, server) in &previous.servers {
                    if server.source == crate::config_schema::McpServerSource::Skill
                        && !config.servers.contains_key(name)
                    {
                        config.servers.insert(name.clone(), server.clone());
                    }
                }
            }
            self.sync_from_config_with(
                config,
                options.cwd,
                options.agent_dir,
                options.env,
                Some(&mut startup_connects),
            )
            .await?;
            // Upstream `attachSession` registers tools once per attach when a `pi` is
            // supplied; the host binds that seam with `bind_registration`.
            self.register_bound_session_tools().await?;
            if mode == Some(maho_ext_api::ExtensionMode::Rpc) {
                let _ = self.capture_wire_status(session_id.as_deref(), None).await;
            }
            Ok::<_, McpServiceError>(())
        }
        .await;
        if let Some((window_session_id, startup_window)) = window {
            if let Some(weak) = self.self_weak.clone() {
                tokio::spawn(async move {
                    startup_window
                        .close(startup_connects, move |release| async move {
                            if let Some(service) = weak.upgrade() {
                                service
                                    .lock()
                                    .await
                                    .release_startup_window(&window_session_id, release)
                                    .await;
                            }
                        })
                        .await;
                });
            } else if startup_connects.is_empty() {
                let mut kind = None;
                startup_window
                    .close(Vec::new(), |release| {
                        kind = Some(release);
                        async {}
                    })
                    .await;
                if let Some(release) = kind {
                    self.release_startup_window(&window_session_id, release)
                        .await;
                }
            }
        }
        result
    }
    /// Re-runs the last sync, so a server whose references changed reconnects under its new identity.
    pub fn resync_current_config(
        &mut self,
    ) -> futures::future::BoxFuture<'_, Result<(), McpServiceError>> {
        Box::pin(async move {
            let (Some(config), Some(cwd), Some(agent_dir)) = (
                self.config.clone(),
                self.session_cwd.clone(),
                self.agent_dir.clone(),
            ) else {
                return Ok(());
            };
            let env = self.session_env.clone();
            self.sync_from_config(config, &cwd, &agent_dir, &env)
                .await?;
            self.register_bound_session_tools().await
        })
    }
    async fn sync_from_config(
        &mut self,
        config: ResolvedMcpConfig,
        cwd: &Path,
        agent_dir: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<(), McpServiceError> {
        self.sync_from_config_with(config, cwd, agent_dir, env, None)
            .await
    }
    async fn sync_from_config_with(
        &mut self,
        config: ResolvedMcpConfig,
        cwd: &Path,
        agent_dir: &Path,
        env: &BTreeMap<String, String>,
        mut startup_connects: Option<&mut Vec<futures::future::BoxFuture<'static, ()>>>,
    ) -> Result<(), McpServiceError> {
        // Every wanted server's references resolve into this sync's connection generation; a failing server
        // is reported and stays unconnected without aborting its siblings (config-generation.ts).
        let mut wanted = config
            .servers
            .iter()
            .filter(|(_, server)| server.state == McpServerState::Enabled)
            .map(|(name, server)| (name.clone(), server.clone()))
            .collect::<BTreeMap<_, _>>();
        let generations = crate::config_generation::resolve_wanted_mcp_generations(
            &mut wanted,
            Some(env),
            Some(agent_dir),
            Some(agent_dir),
            &mut self.reference_errors,
        );
        let mut config = config;
        for (name, error) in &self.reference_errors {
            let diagnostic = format!("MCP server '{name}' skipped: {error}");
            if !config.diagnostics.contains(&diagnostic) {
                config.diagnostics.push(diagnostic);
            }
        }
        let generation_hash = |name: &str, declared: &str| {
            crate::config_generation::mcp_connection_identity(
                declared,
                generations
                    .get(name)
                    .map(|generation| generation.config_hash.as_str()),
            )
        };
        // service.ts `credentialIdentities`: the credential each wanted server would connect with; it keys the
        // shared transport, so a changed credential never reuses a connection admitted under the old one.
        let credential_identity = |name: &str| {
            config
                .servers
                .get(name)
                .and_then(|server| server.config.as_ref())
                .and_then(|declared| {
                    crate::auth::catalog_identity::mcp_credential_identity(
                        declared,
                        name,
                        Some(agent_dir),
                        Some(env),
                    )
                })
        };
        let existing = self.connections.keys().cloned().collect::<Vec<_>>();
        for name in existing {
            let entry = self.connections[&name].entry.lock().await;
            let retain = config.servers.get(&name).is_some_and(|server| {
                server.state == McpServerState::Enabled
                    && server
                        .config_hash
                        .as_deref()
                        .map(|declared| generation_hash(&name, declared))
                        .as_deref()
                        == Some(&entry.config_hash)
                    && generations.get(&name).is_some_and(|generation| {
                        entry.key
                            == crate::sharing_policy::shared_mcp_key(
                                &name,
                                &generation.config,
                                Some(env),
                                credential_identity(&name).as_deref(),
                                &agent_dir.to_string_lossy(),
                                &cwd.to_string_lossy(),
                            )
                    })
            }) && self.agent_dir.as_deref() == Some(agent_dir)
                // service.ts:782: a connection whose credentials went stale is re-created, never kept under an
                // unchanged key; kept, it would refuse every request until the service is disposed.
                && (self.connections[&name].credentials_current)();
            drop(entry);
            if !retain && let Some(connection) = self.connections.remove(&name) {
                dispose_entry_connection(&connection, &self.registry, self.owner).await?;
            }
        }
        let race_window_closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut races = Vec::new();
        for (name, server) in &config.servers {
            if server.state != McpServerState::Enabled || self.connections.contains_key(name) {
                continue;
            }
            let (Some(generation), Some(declared_hash)) =
                (generations.get(name), &server.config_hash)
            else {
                continue;
            };
            let server_config = &generation.config;
            let hash = &generation_hash(name, declared_hash);
            let key = crate::sharing_policy::shared_mcp_key(
                name,
                server_config,
                Some(env),
                credential_identity(name).as_deref(),
                &agent_dir.to_string_lossy(),
                &cwd.to_string_lossy(),
            );
            let mut connection = create_shared_mcp_session_connection(
                SessionConnectionOptions {
                    registry: &self.registry,
                    owner: self.owner,
                    key: &key,
                    name,
                    config_hash: hash,
                    config: server_config.clone(),
                    declared: server.config.as_ref(),
                    agent_dir,
                    env: Some(env.clone()),
                    elicitation_ui: self.elicitation_ui.clone(),
                    elicitation_enabled: self.elicitation_enabled,
                },
                &cwd.to_string_lossy(),
            )?;
            // service.ts `credentialsCurrent`: the identity this connection is admitted under is still live.
            if let Some(declared) = server.config.clone() {
                let admitted = credential_identity(name);
                let (name, agent_dir, env) = (name.clone(), agent_dir.to_path_buf(), env.clone());
                connection.credentials_current = Arc::new(move || {
                    crate::auth::catalog_identity::mcp_credential_identity(
                        &declared,
                        &name,
                        Some(&agent_dir),
                        Some(&env),
                    ) == admitted
                });
            }
            {
                let mut leases = self
                    .binding
                    .leases
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match &connection.shared {
                    Some(lease) => {
                        leases.insert(name.clone(), Arc::downgrade(lease));
                    }
                    None => {
                        leases.remove(name);
                    }
                }
            }
            {
                let mut entry = connection.entry.lock().await;
                entry
                    .connection
                    .set_elicitation_ui(self.elicitation_ui.clone());
                entry.artifacts = Some(self.output_artifacts.clone());
                // A server that declares references re-reads them for every transport; a changed snapshot
                // re-keys the whole sync (service.ts generationChanged), never reusing the stale identity.
                if let Some(declared) = &server.config {
                    let weak = self.self_weak.clone();
                    let own_rekey: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                        let Some(service) = weak.as_ref().and_then(std::sync::Weak::upgrade) else {
                            return;
                        };
                        tokio::spawn(async move {
                            let _ = service.lock().await.resync_current_config().await;
                        });
                    });
                    // shared-connection.ts: a pooled connection's changed snapshot re-keys every lease.
                    let rekey = match &connection.shared {
                        Some(lease) => {
                            lease.shared.on_generation_changed(self.owner, own_rekey);
                            let shared = Arc::downgrade(&lease.shared);
                            Arc::new(move || {
                                if let Some(shared) = shared.upgrade() {
                                    shared.notify_generation_changed();
                                }
                            }) as Arc<dyn Fn() + Send + Sync>
                        }
                        None => own_rekey,
                    };
                    entry.connection.set_resolve_config(
                        crate::config_generation::mcp_generation_renewal(
                            crate::config_generation::McpGenerationScope {
                                server_name: name.clone(),
                                declared: declared.clone(),
                                env: Some(env.clone()),
                                agent_dir: Some(agent_dir.to_path_buf()),
                            },
                            &generation.config_hash,
                            rekey,
                        ),
                    );
                }
            }
            let cache = crate::catalog_cache::read_mcp_catalog_cache(agent_dir);
            // catalog-cache-adoption.ts: the keyed identity's entry, or one an earlier unkeyed build wrote for the
            // same config and credential (migrated under the keyed identities). A server without a credential
            // identity reads the plain keyed entry.
            let now = chrono::Utc::now().timestamp_millis() as f64;
            let credential_identity = server.config.as_ref().and_then(|declared| {
                crate::auth::catalog_identity::mcp_credential_identity(
                    declared,
                    name,
                    Some(agent_dir),
                    Some(env),
                )
            });
            let cached_catalog = match (&credential_identity, &server.config) {
                // An authenticated server whose credential is unavailable reads no catalog.
                (None, Some(_)) => None,
                (Some(credential_identity), Some(declared)) => {
                    crate::catalog_cache_adoption::adopt_mcp_cached_server(
                        crate::catalog_cache_adoption::McpCacheAdoption {
                            cache: &cache,
                            agent_dir: Some(agent_dir),
                            server_name: name,
                            current: crate::catalog_cache_adoption::McpCacheIdentity {
                                config_hash: hash.clone(),
                                credential_identity: credential_identity.clone(),
                            },
                            legacy: Box::new(|| {
                                crate::catalog_cache_adoption::legacy_mcp_cache_identity(
                                    declared,
                                    name,
                                    Some(agent_dir),
                                    Some(env),
                                )
                            }),
                        },
                        now,
                    )
                }
                _ => {
                    crate::catalog_cache::get_valid_cached_server(&cache, name, hash, now).cloned()
                }
            };
            if let Some(cached) = &cached_catalog {
                connection.entry.lock().await.cached_catalog = Some(cached.clone());
            }
            let credentials_current = connection.credentials_current.clone();
            let entry_handle = connection.entry.clone();
            let shared_handle = connection.shared.clone();
            self.connections.insert(name.clone(), connection);
            self.wire_list_changed(name).await;
            // Upstream races every startup connect bounded by the race deadline
            // (`shouldRaceMcpStartup(lifecycle) || cachedCatalog === undefined`): a cold
            // lazy server with no cached catalog also races so its catalog lands and the
            // deferred registration below publishes its tools. A cached lazy server needs
            // no startup connect: its tools come from the cache and it connects on demand.
            if crate::startup_race::should_race_mcp_startup(
                server_config
                    .lifecycle
                    .unwrap_or(crate::config_schema::Lifecycle::Lazy),
            ) || cached_catalog.is_none()
            {
                let entry = entry_handle;
                let server_config = server_config.clone();
                let shared = shared_handle;
                let self_weak = self.self_weak.clone();
                let registration_inputs = self.registration_inputs.clone();
                let deferred_registration = self.deferred_registration.clone();
                let race_window_closed = race_window_closed.clone();
                let timeout = crate::startup_race::resolve_mcp_startup_timeout_ms(
                    server_config.startup_timeout_ms,
                    env.get(crate::startup_race::MCP_STARTUP_TIMEOUT_ENV)
                        .map(String::as_str),
                );
                let (sender, settled) = tokio::sync::watch::channel(false);
                let (reg_sender, mut reg_settled) = tokio::sync::watch::channel(false);
                self.deferred.track(async move {
                    if let Some(shared) = shared {
                        match shared.catalog().await {
                            Ok(catalog) => {
                                let mut entry = entry.lock().await;
                                entry.cached_catalog = Some(catalog);
                                entry.cache_refreshed_after_connect = true;
                            }
                            Err(error) => {
                                let (connection, logger) = {
                                    let entry = entry.lock().await;
                                    (entry.connection.clone(), entry.logger.clone())
                                };
                                let error = crate::health::mark_mcp_connection_needs_auth(
                                    &connection,
                                    &error,
                                )
                                .unwrap_or(error);
                                let _ = logger
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .log("warning", &error.to_string(), None, None);
                            }
                        }
                    } else if let Err(error) =
                        crate::startup_race::connect_and_refresh_mcp_catalog_unlocked(
                            &entry,
                            &server_config,
                        )
                        .await
                    {
                        let logger = entry.lock().await.logger.clone();
                        let _ = logger
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .log("warning", &error.to_string(), None, None);
                    }
                    // Fast connects are covered by the attach's own registration. Publish late
                    // catalogs (or resync stale credentials) outside the attach's service lock.
                    let credentials_stale = !(credentials_current)();
                    let settled_after_race =
                        race_window_closed.load(std::sync::atomic::Ordering::SeqCst);
                    if (credentials_stale || (settled_after_race && registration_inputs.is_some()))
                        && let Some(service) = self_weak.as_ref().and_then(std::sync::Weak::upgrade)
                    {
                        deferred_registration.track(async move {
                            let mut service = service.lock().await;
                            if !(credentials_current)() {
                                if let Err(error) = service.resync_current_config().await {
                                    service.report_registration_error(&error.to_string());
                                }
                            } else if settled_after_race && let Some(inputs) = registration_inputs {
                                let mut tool_search = match &inputs.tool_search {
                                    Some(tool_search) => Some(tool_search.lock().await),
                                    None => None,
                                };
                                if let Err(error) = service
                                    .register_session_tools(
                                        inputs.registrar.clone(),
                                        tool_search.as_deref_mut(),
                                    )
                                    .await
                                {
                                    service.report_registration_error(&error.to_string());
                                }
                            }
                            reg_sender.send_replace(true);
                        });
                    } else {
                        reg_sender.send_replace(true);
                    }
                    // Settle the race after enqueuing follow-up work, not after it takes the lock.
                    sender.send_replace(true);
                });
                if let Some(connects) = startup_connects.as_deref_mut() {
                    connects.push(Box::pin(async move {
                        while !*reg_settled.borrow() {
                            if reg_settled.changed().await.is_err() {
                                break;
                            }
                        }
                    }));
                }
                races.push((timeout, settled));
            }
        }
        // PERF-DIVERGENCE(senpi mcp service.ts): upstream wires list-changed before racing and runs every
        // startup race concurrently (Promise.all). The earlier port raced each server in turn and wired
        // list-changed after the race, so wire_list_changed queued on the entry lock the connect task holds
        // and waited out each whole connect (~5s at startup). Same wiring and deadlines; only ordering/overlap.
        let _ =
            futures::future::join_all(races.into_iter().map(|(timeout, mut settled)| async move {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs_f64(timeout / 1000.0),
                    async {
                        while !*settled.borrow() {
                            if settled.changed().await.is_err() {
                                break;
                            }
                        }
                    },
                )
                .await;
            }))
            .await;
        race_window_closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut declared = BTreeMap::new();
        for (name, connection) in &self.connections {
            let entry = connection.entry.lock().await;
            declared.insert(
                name.clone(),
                McpDeclaredServer {
                    connection: Arc::downgrade(&entry.connection),
                    credentials_current: connection.credentials_current.clone(),
                },
            );
        }
        *self
            .binding
            .declared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = declared;
        self.agent_dir = Some(agent_dir.into());
        self.session_cwd = Some(cwd.into());
        self.session_env = env.clone();
        *self
            .binding
            .sources
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = config
            .servers
            .iter()
            .map(|(name, server)| (name.clone(), server.source))
            .collect();
        self.config = Some(config);
        self.native_tool_search_gate.publish(
            self.config
                .as_ref()
                .and_then(|config| config.settings.native_tool_search.as_ref()),
        );
        Ok(())
    }
    pub async fn attach_skill_mcp_servers(
        &mut self,
        declared: &crate::skills::SkillMcpDeclarations,
    ) -> Result<Vec<String>, McpServiceError> {
        let (Some(mut config), Some(cwd), Some(agent_dir)) = (
            self.config.clone(),
            self.session_cwd.clone(),
            self.agent_dir.clone(),
        ) else {
            return Ok(Vec::new());
        };
        let mut warnings = Vec::new();
        let mut changed = false;
        for (name, decl) in &declared.servers {
            let existing = config.servers.get(name);
            if let Some(existing) = existing
                .filter(|server| server.source != crate::config_schema::McpServerSource::Skill)
            {
                warnings.push(format!("MCP server '{name}' from skill {} collides with the {} config; system config wins.",decl.source_path.display(),existing.source));
                continue;
            }
            let project = decl
                .provenance
                .as_ref()
                .map_or(decl.scope.as_deref() == Some("project"), |provenance| {
                    provenance.kind != "user"
                });
            if project
                && let Some(existing) = existing
                && crate::project_skill_policy::reuse_project_skill_server(
                    name, decl, &cwd, &agent_dir, existing,
                )?
            {
                continue;
            }
            let project_skill = decl
                .provenance
                .as_ref()
                .map_or(decl.scope.as_deref() == Some("project"), |provenance| {
                    provenance.kind != "user"
                });
            // The project environment policy materializes plain strings only, so a project skill server that
            // declares a reference object is refused before any I/O; skill-server.ts refuses the remote and
            // untrusted ones itself (senpi#3858).
            if project_skill
                && let Some(warning) = crate::config_references::reference_refusal(
                    name,
                    &decl.raw,
                    &format!("Skill '{}' MCP server '{name}'", decl.skill_name),
                )
            {
                if self.skill_server_warnings.insert(warning.clone()) {
                    warnings.push(warning);
                }
                continue;
            }
            let resolved = if project_skill {
                crate::project_skill_policy::prepare_project_skill_server_with_notices(
                    name,
                    decl,
                    &cwd,
                    &agent_dir,
                    self.project_trusted,
                    &self.session_env,
                    (
                        self.elicitation_ui.clone(),
                        self.policy_notices.clone(),
                        self.policy_notice_event.clone(),
                    ),
                )
                .await?
            } else {
                // skill-server.ts trust: a skill without a user scope carries the project's trust.
                let trusted = match decl.scope.as_deref() {
                    None | Some("project") => self.project_trusted,
                    Some(_) => true,
                };
                let result = crate::skill_server::resolve_skill_mcp_server(
                    name,
                    &decl.raw,
                    &decl.source_path,
                    &crate::skill_server::SkillServerSource {
                        skill_name: &decl.skill_name,
                        trusted,
                        env: &self.session_env,
                    },
                    Some(&agent_dir),
                )?;
                if let Some(warning) = result.warning
                    && self.skill_server_warnings.insert(warning.clone())
                {
                    warnings.push(warning);
                }
                let Some(server) = result.server else {
                    continue;
                };
                server
            };
            if existing.is_some_and(|existing| existing.config_hash == resolved.config_hash) {
                continue;
            }
            config.servers.insert(name.clone(), resolved);
            changed = true;
        }
        if changed {
            let env = self.session_env.clone();
            self.sync_from_config(config, &cwd, &agent_dir, &env)
                .await?;
            self.register_bound_session_tools().await?;
        }
        Ok(warnings)
    }
    async fn register_bound_session_tools(&mut self) -> Result<(), McpServiceError> {
        let Some(inputs) = self.registration_inputs.clone() else {
            return Ok(());
        };
        let mut tool_search = match &inputs.tool_search {
            Some(tool_search) => Some(tool_search.lock().await),
            None => None,
        };
        self.register_session_tools(inputs.registrar.clone(), tool_search.as_deref_mut())
            .await
    }
    /// Upstream `#wireListChanged`: coalesce the connection's tools-changed signal and re-register through the
    /// bound seam (service-tools-changed.ts `subscribeMcpToolsChanged`). The refresh upgrades the service's
    /// `Weak` self-handle, so the subscription never holds the session lock.
    async fn wire_list_changed(&self, name: &str) {
        let Some(connection) = self.connections.get(name) else {
            return;
        };
        let entry_handle = connection.entry.clone();
        let mut entry = entry_handle.lock().await;
        let weak = self.self_weak.clone();
        let name_owned = name.to_owned();
        let sink = crate::wrap::McpAsyncErrorSink::from_logger(entry.logger.clone());
        let (task, coalescer) = crate::service_tools_changed::subscribe_mcp_tools_changed(
            &entry,
            move |connect_only| {
                let weak = weak.clone();
                let name = name_owned.clone();
                async move {
                    let Some(service) = weak.as_ref().and_then(std::sync::Weak::upgrade) else {
                        return Ok(());
                    };
                    let mut service = service.lock().await;
                    service
                        .handle_server_tools_changed_with(&name, connect_only)
                        .await
                        .map_err(|error| {
                            crate::errors::McpError::new(
                                crate::errors::McpErrorKind::ToolExec,
                                error.to_string(),
                            )
                        })
                }
            },
            sink,
        );
        entry.list_changed_tasks.push(task);
        entry.list_changed_coalescer = Some(coalescer);
        let state_weak = self.self_weak.clone();
        let mut state_changes = entry.connection.on_state_change();
        let state_task = tokio::spawn(async move {
            loop {
                match state_changes.recv().await {
                    Ok(_) => {
                        let Some(service) = state_weak.as_ref().and_then(std::sync::Weak::upgrade)
                        else {
                            return;
                        };
                        let mut service = service.lock().await;
                        if service.session_mode == Some(maho_ext_api::ExtensionMode::Rpc) {
                            let session_id = service.binding.session_id();
                            let _ = service
                                .refresh_wire_status_snapshot(session_id.as_deref())
                                .await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        });
        entry.list_changed_tasks.push(state_task);
    }
    /// Upstream `#handleServerToolsChanged` for a reported change. Public so tests drive the same entry point.
    pub async fn handle_server_tools_changed(&mut self, name: &str) -> Result<(), McpServiceError> {
        self.handle_server_tools_changed_with(name, false).await
    }
    /// Upstream `#handleServerToolsChanged`: refresh the server's catalog (service-tools-changed.ts
    /// `refreshMcpToolsOnListChanged`); a connect-only refresh leaves an unchanged registration in place.
    pub async fn handle_server_tools_changed_with(
        &mut self,
        name: &str,
        connect_only: bool,
    ) -> Result<(), McpServiceError> {
        let Some(config) = self.config.clone() else {
            return Ok(());
        };
        let Some(connection) = self.connections.get(name) else {
            return Ok(());
        };
        let entry = connection.entry.clone();
        let shared = connection.shared.clone();
        let server_connection = entry.lock().await.connection.clone();
        let binding = self.binding.clone();
        let is_current = {
            let (binding, name, connection) =
                (binding.clone(), name.to_owned(), server_connection.clone());
            Arc::new(move || binding.declares_connection(&name, &connection))
                as Arc<dyn Fn() -> bool + Send + Sync>
        };
        let weak = self.self_weak.clone();
        let scope = crate::service_tools_changed::McpToolsRefreshScope {
            is_current,
            credentials_current: binding.credentials_current(name),
            // Re-key from the live state; the service lock is held here, so the resync runs after it.
            on_credentials_changed: Some(Box::new(move || {
                Box::pin(async move {
                    if let Some(service) = weak.as_ref().and_then(std::sync::Weak::upgrade) {
                        tokio::spawn(async move {
                            let _ = service.lock().await.resync_current_config().await;
                        });
                    }
                })
            })),
            startup_owns_registration: false,
            shared,
        };
        let registered_identity = self
            .registration
            .as_ref()
            .and_then(|registration| registration.registered_identities.get(name).cloned());
        // No bound registrar means no session registered this catalog: nothing to re-register.
        let registrar = self
            .registration_inputs
            .as_ref()
            .map(|inputs| inputs.registrar.clone());
        let targets = registrar
            .map(
                |registrar| crate::service_tools_changed::McpToolsRefreshTarget {
                    registrar,
                    registered_identity,
                    register: Box::new(move || {
                        Box::pin(async move {
                            self.register_bound_session_tools().await.map_err(|error| {
                                crate::errors::McpError::new(
                                    crate::errors::McpErrorKind::ToolExec,
                                    error.to_string(),
                                )
                            })
                        })
                    }),
                },
            )
            .into_iter()
            .collect();
        crate::service_tools_changed::refresh_mcp_tools_on_list_changed(
            &entry,
            targets,
            &config,
            connect_only,
            scope,
        )
        .await?;
        Ok(())
    }
    pub async fn register_session_tools(
        &mut self,
        registrar: Arc<dyn crate::tool_registrar::McpToolRegistrar>,
        tool_search: Option<&mut maho_ext_tool_search::service::ToolSearchService>,
    ) -> Result<(), McpServiceError> {
        let Some(config) = self.config.clone() else {
            return Ok(());
        };
        let entries = self
            .connections
            .values()
            .map(|connection| connection.entry.clone())
            .collect::<Vec<_>>();
        let refresh_active_set_when_empty = !self.tier_b_registry.managed_names.is_empty();
        self.registration = crate::service_register::register_mcp_service_direct_tools(
            registrar,
            &config,
            &entries,
            tool_search,
            &mut self.tier_b_registry,
            refresh_active_set_when_empty,
            Some(&self.binding),
        )
        .await?;
        if let Some(registration) = &self.registration {
            for error in &registration.wiring_errors {
                self.report_registration_error(error);
            }
        }
        if self.session_mode == Some(maho_ext_api::ExtensionMode::Rpc) {
            let session_id = self.binding.session_id();
            let _ = self.capture_wire_status(session_id.as_deref(), None).await;
        }
        // Upstream `for (const listener of this.#registrationListeners) listener()`.
        self.registration_events
            .send_modify(|generation| *generation += 1);
        Ok(())
    }
    /// Upstream `onMcpRegistrationChanged`: a receiver whose `changed()` fires after each completed
    /// catalog registration. Dropping it is the unsubscribe.
    pub fn on_registration_changed(&self) -> tokio::sync::watch::Receiver<u64> {
        self.registration_events.subscribe()
    }
    /// The state-change stream of every live connection (upstream `getConnection(name).onStateChange`).
    pub async fn connection_state_changes(
        &self,
    ) -> Vec<tokio::sync::broadcast::Receiver<crate::connection::ServerConnectionStateChangedEvent>>
    {
        let mut receivers = Vec::new();
        for name in self.connections.keys() {
            if let Some(connection) = self.get_connection(name) {
                receivers.push(connection.entry.lock().await.connection.on_state_change());
            }
        }
        receivers
    }
    /// The client of a connection whose state is `connected` (upstream `connection?.state === "connected"`).
    pub async fn connected_client(
        &self,
        name: &str,
    ) -> Option<Arc<crate::transport_sdk::McpClient>> {
        let entry = self.get_connection(name)?.entry.lock().await;
        if entry.connection.state() != crate::connection::ServerConnectionState::Connected {
            return None;
        }
        entry.connection.client().ok()
    }
    /// Upstream `getServerExposureStatus`.
    pub async fn exposure_status(
        &self,
        name: &str,
    ) -> crate::expose::status::McpServerExposureStatus {
        // Upstream `#entryForName`: unfiltered, unlike `getConnection`.
        let entry = match self.connections.get(name) {
            Some(connection) => Some(connection.entry.lock().await),
            None => None,
        };
        crate::service_exposure::get_mcp_service_exposure_status(
            name,
            self.config.as_ref(),
            entry.as_deref(),
        )
        .await
    }
    /// Upstream `getServerAuthStatus` (`wireAuthStatus`): bearer servers in `needs_auth` are not logged in;
    /// OAuth servers are logged in when their credential identity resolves (a non-empty access token).
    pub async fn server_auth_status(&self, name: &str) -> crate::service_types::McpWireAuthStatus {
        use crate::service_types::McpWireAuthStatus;
        let Some(server) = self
            .config
            .as_ref()
            .and_then(|config| config.servers.get(name))
        else {
            return McpWireAuthStatus::Unsupported;
        };
        // Upstream `#entryForName`: unfiltered, unlike `getConnection`.
        let state = match self.connections.get(name) {
            Some(connection) => Some(connection.entry.lock().await.connection.state()),
            None => None,
        };
        match server
            .config
            .as_ref()
            .map(crate::auth::context::resolve_auth_mode)
        {
            Some(crate::auth::context::ServerAuthMode::Bearer) => {
                if state == Some(crate::connection::ServerConnectionState::NeedsAuth) {
                    McpWireAuthStatus::NotLoggedIn
                } else {
                    McpWireAuthStatus::BearerToken
                }
            }
            // Upstream `mcpCredentialIdentity(...) === undefined`: a read-only probe (never migrates or
            // writes the store) where an empty accessToken is not signed in.
            Some(crate::auth::context::ServerAuthMode::OAuth) => {
                let logged_in = server.config.as_ref().is_some_and(|config| {
                    crate::auth::catalog_identity::mcp_credential_identity(
                        config,
                        name,
                        self.agent_dir.as_deref(),
                        Some(&self.session_env),
                    )
                    .is_some()
                });
                if logged_in {
                    McpWireAuthStatus::OAuth
                } else {
                    McpWireAuthStatus::NotLoggedIn
                }
            }
            _ => McpWireAuthStatus::Unsupported,
        }
    }
    fn report_registration_error(&self, message: &str) {
        if let Some(agent_dir) = &self.agent_dir
            && let Ok(mut logger) = crate::log::McpLogger::new("service", agent_dir, None)
        {
            let _ = logger.log("error", message, None, None);
        }
        maho_ext_api::diagnostic(&format!("MCP registration: {message}"));
    }
    pub fn registration(&self) -> Option<&crate::expose::session::McpSessionRegistration> {
        self.registration.as_ref()
    }
    /// Resolved `settings.nativeToolSearch` (`auto | true | false | undefined`), the port of
    /// upstream `getNativeToolSearchSetting`.
    pub fn native_tool_search_setting(&self) -> Option<crate::config_schema::NativeToolSearch> {
        self.config
            .as_ref()
            .and_then(|config| config.settings.native_tool_search.clone())
    }
    /// Shared gate handle the host binds into the shared tool-search adapter. Cloning keeps
    /// every holder observing later session attachments, exactly like the upstream module gate.
    pub fn native_tool_search_gate(&self) -> McpNativeToolSearchGate {
        self.native_tool_search_gate.clone()
    }
    pub async fn connect_server(&self, name: &str) -> Result<(), McpServiceError> {
        if !self.connections.contains_key(name) {
            return Err(crate::errors::McpError::new(
                crate::errors::McpErrorKind::Connect,
                format!("Unknown MCP server: {name}"),
            )
            .into());
        }
        if let (Some(connection), Some(config)) = (
            self.connections.get(name),
            self.config
                .as_ref()
                .and_then(|config| config.servers.get(name))
                .and_then(|server| server.config.as_ref()),
        ) {
            let mut entry = connection.entry.lock().await;
            if let Some(shared) = &connection.shared {
                entry.cached_catalog = Some(shared.catalog().await?);
                entry.cache_refreshed_after_connect = true;
                return Ok(());
            }
            crate::startup_race::connect_and_refresh_mcp_catalog(&mut entry, config).await?;
            if let Some(error) = entry.connection.last_error() {
                return Err(error.into());
            }
        }
        Ok(())
    }
    pub async fn reconnect_server(&self, name: &str) -> Result<(), McpServiceError> {
        let connection = self.connections.get(name).ok_or_else(|| {
            crate::errors::McpError::new(
                crate::errors::McpErrorKind::Connect,
                format!("Unknown MCP server: {name}"),
            )
        })?;
        if let Some(shared) = &connection.shared {
            shared.renew().await?;
        } else if let Some(reconnect) = &connection.reconnect {
            reconnect.reconnect_now().await?;
        }
        Ok(())
    }
    /// Upstream `getAuthTarget`: what `/mcp auth*` needs for a configured server.
    pub fn auth_target(&self, name: &str) -> Option<McpAuthTarget> {
        let config = self.config.as_ref()?;
        let server = config.servers.get(name)?.config.clone()?;
        Some(McpAuthTarget {
            config: server,
            agent_dir: self.agent_dir.clone()?,
            callback_url: config.settings.oauth_callback_url.clone(),
        })
    }
    /// Upstream `getPendingAuth`.
    pub fn pending_auth(
        &self,
    ) -> Arc<std::sync::Mutex<BTreeMap<String, crate::auth::oauth_provider::McpOAuthProvider>>>
    {
        self.pending_auth.clone()
    }
    /// Upstream `beginInteractiveAuth`/`endInteractiveAuth`: one browser flow per server per service.
    pub fn interactive_auth_guard(&self) -> crate::auth::commands_auth::McpInteractiveAuthGuard {
        let begin = self.interactive_auth_servers.clone();
        let end = self.interactive_auth_servers.clone();
        crate::auth::commands_auth::McpInteractiveAuthGuard {
            begin: Arc::new(move |name| {
                begin
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(name.to_owned())
            }),
            end: Arc::new(move |name| {
                end.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(name);
            }),
        }
    }
    /// Cheap handle to the deferred-attach tracker so callers can wait without holding the service lock.
    pub fn deferred_attach(&self) -> Arc<crate::startup_race::McpDeferredAttach> {
        self.deferred.clone()
    }
    pub fn deferred_registration(&self) -> Arc<crate::startup_race::McpDeferredAttach> {
        self.deferred_registration.clone()
    }
    pub async fn wait_for_deferred_attach(
        &self,
        timeout: std::time::Duration,
    ) -> crate::startup_race::McpStartupRaceResult {
        self.deferred.wait(timeout).await
    }
    pub async fn when_attach_settled(&self) {
        let _ = self
            .deferred
            .wait(std::time::Duration::from_millis(
                crate::startup_race::MCP_ATTACH_SETTLE_TIMEOUT_MS,
            ))
            .await;
    }
    /// Upstream `getConnection`: a connection whose credentials went stale is hidden until the next sync
    /// re-creates it (service.ts:669).
    pub fn get_connection(&self, name: &str) -> Option<&McpSessionConnection> {
        self.connections
            .get(name)
            .filter(|connection| (connection.credentials_current)())
    }
    /// Upstream `#serverSnapshot`: a stale-credential connection is hidden; a server whose references failed is
    /// degraded with the reference error; a `needs_auth` connection whose credential identity is gone reports
    /// `needs_auth` with its own last error.
    fn server_snapshot(
        &self,
        name: &str,
        server: Option<&crate::config_schema::ResolvedMcpServer>,
        connection: Option<&McpSessionConnection>,
        entry: Option<&crate::service_types::McpConnectionEntry>,
        now: f64,
    ) -> McpServerSnapshot {
        let current = connection.is_none_or(|connection| (connection.credentials_current)());
        let mut snapshot = crate::service_snapshot::build_mcp_server_snapshot(
            name,
            server,
            entry
                .filter(|_| current)
                .map(|entry| entry.connection.as_ref()),
            entry,
            now,
        );
        let redact = |message: &str| {
            maho_core::config_reference_redaction::redact_resolved_reference_values(
                message,
                "<redacted>",
            )
        };
        if entry.is_none()
            && let Some(error) = self.reference_errors.get(name)
        {
            snapshot.lifecycle_state = crate::service_types::McpLifecycleState::Connection(
                crate::connection::ServerConnectionState::Degraded,
            );
            snapshot.last_error = Some(redact(&error.to_string()));
            return snapshot;
        }
        if let (Some(config), Some(entry)) =
            (server.and_then(|server| server.config.as_ref()), entry)
            && entry.connection.state() == crate::connection::ServerConnectionState::NeedsAuth
            && crate::auth::catalog_identity::mcp_credential_identity(
                config,
                name,
                entry.agent_dir.as_deref(),
                Some(&self.session_env),
            )
            .is_none()
        {
            snapshot.lifecycle_state = crate::service_types::McpLifecycleState::Connection(
                crate::connection::ServerConnectionState::NeedsAuth,
            );
            snapshot.last_error = entry
                .connection
                .last_error()
                .map(|error| redact(&error.message));
        }
        snapshot
    }
    pub async fn server_snapshots(&self) -> Vec<McpServerSnapshot> {
        let Some(config) = &self.config else {
            return Vec::new();
        };
        let mut snapshots = Vec::new();
        for (name, server) in &config.servers {
            let connection = self.connections.get(name);
            let entry = match connection {
                Some(connection) => Some(connection.entry.lock().await),
                None => None,
            };
            snapshots.push(self.server_snapshot(
                name,
                Some(server),
                connection,
                entry.as_deref(),
                chrono::Utc::now().timestamp_millis() as f64,
            ));
        }
        snapshots
    }
    pub async fn status(&self, title: &str) -> String {
        let mut rows = Vec::new();
        for snapshot in self.server_snapshots().await {
            let entry = if let Some(connection) = self.connections.get(&snapshot.name) {
                Some(connection.entry.lock().await)
            } else {
                None
            };
            let exposure = crate::service_exposure::get_mcp_service_exposure_status(
                &snapshot.name,
                self.config.as_ref(),
                entry.as_deref(),
            )
            .await;
            rows.push(crate::status::McpStatusRow { snapshot, exposure });
        }
        crate::status::format_mcp_status(title, &rows)
    }
    pub async fn wire_status_snapshot(&self) -> crate::service_types::McpWireStatusSnapshot {
        use crate::service_types::*;
        let mut snapshot = McpWireStatusSnapshot::default();
        let Some(config) = &self.config else {
            return snapshot;
        };
        for (name, server) in &config.servers {
            let session_connection = self.connections.get(name);
            let entry = match session_connection {
                Some(connection) => Some(connection.entry.lock().await),
                None => None,
            };
            // service.ts #captureWireStatusServer: a stale-credential connection and its cached catalog are hidden.
            let current =
                session_connection.is_none_or(|connection| (connection.credentials_current)());
            let mode = server
                .config
                .as_ref()
                .map(crate::auth::context::resolve_auth_mode);
            let auth_status = match mode {
                Some(crate::auth::context::ServerAuthMode::Bearer) => {
                    McpWireAuthStatus::BearerToken
                }
                Some(crate::auth::context::ServerAuthMode::OAuth) => {
                    let logged_in = self
                        .agent_dir
                        .as_ref()
                        .zip(server.config.as_ref())
                        .and_then(|(dir, config)| {
                            config.url.as_ref().map(|url| {
                                crate::auth::token_store::McpTokenStore::new(dir, name, url)
                            })
                        })
                        .and_then(|store| store.read().ok().flatten())
                        .is_some_and(|record| record.access_token.is_some());
                    if logged_in {
                        McpWireAuthStatus::OAuth
                    } else {
                        McpWireAuthStatus::NotLoggedIn
                    }
                }
                _ => McpWireAuthStatus::Unsupported,
            };
            // service.ts `#captureWireStatusServer`: the diagnostic snapshot's lastError and lifecycle decide the
            // reported status: needs_auth while not connected, degraded for a reference failure without a
            // connection, otherwise the visible connection's state or the configured state.
            let diagnostic = self.server_snapshot(
                name,
                Some(server),
                session_connection,
                entry.as_deref(),
                0.0,
            );
            let connection_state = entry
                .as_ref()
                .filter(|_| current)
                .map(|entry| entry.connection.state());
            let connected =
                connection_state == Some(crate::connection::ServerConnectionState::Connected);
            let lifecycle =
                |state| diagnostic.lifecycle_state == McpLifecycleState::Connection(state);
            let status = if !connected
                && lifecycle(crate::connection::ServerConnectionState::NeedsAuth)
            {
                McpWireServerStatus::Connection(crate::connection::ServerConnectionState::NeedsAuth)
            } else if entry.is_none()
                && lifecycle(crate::connection::ServerConnectionState::Degraded)
            {
                McpWireServerStatus::Connection(crate::connection::ServerConnectionState::Degraded)
            } else {
                connection_state.map_or(
                    McpWireServerStatus::Config(server.state),
                    McpWireServerStatus::Connection,
                )
            };
            let mut wire = McpWireStatusServer {
                name: name.clone(),
                server_info: None,
                last_error: diagnostic.last_error,
                tools: Vec::new(),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                auth_status,
                status: Some(status),
            };
            if let Some(cached) = entry
                .as_ref()
                .filter(|_| current)
                .and_then(|entry| entry.cached_catalog.as_ref())
            {
                wire.tools = cached
                    .tools
                    .iter()
                    .filter_map(|tool| serde_json::from_value(tool.clone()).ok())
                    .collect();
                wire.resources = cached
                    .resources
                    .iter()
                    .filter_map(|resource| serde_json::from_value(resource.clone()).ok())
                    .collect();
            }
            if let Some(client) = entry
                .as_ref()
                .filter(|_| connected)
                .and_then(|entry| entry.connection.client().ok())
            {
                wire.server_info =
                    serde_json::from_value(client.server_info.read().await.clone()).ok();
                let timeout = std::time::Duration::from_secs_f64(
                    server
                        .config
                        .as_ref()
                        .and_then(|config| config.request_timeout_ms)
                        .unwrap_or(30000.0)
                        / 1000.0,
                );
                if let Ok(templates) = crate::catalog::collect_client_pages(
                    &client,
                    "resources/templates/list",
                    "resourceTemplates",
                    timeout,
                )
                .await
                {
                    wire.resource_templates = templates
                        .items
                        .into_iter()
                        .filter_map(|template| serde_json::from_value(template).ok())
                        .collect();
                }
            }
            snapshot.servers.push(wire);
        }
        snapshot
    }
    /// commands.ts `testServer`: connect, list tools, and record the call either way.
    pub async fn test_server(&self, name: &str) -> Result<(f64, usize), McpTestFailure> {
        let started = tokio::time::Instant::now();
        let result = async {
            self.connect_server(name).await?;
            let connection = self.connections.get(name).ok_or_else(|| {
                crate::errors::McpError::new(
                    crate::errors::McpErrorKind::Connect,
                    format!("Unknown MCP server: {name}"),
                )
            })?;
            let client = connection.entry.lock().await.connection.client()?;
            Ok::<_, McpServiceError>(
                client
                    .request(
                        "tools/list",
                        serde_json::json!({}),
                        std::time::Duration::from_secs(2),
                    )
                    .await?
                    .get("tools")
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, Vec::len),
            )
        }
        .await;
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.record_call(name, elapsed_ms, result.is_err()).await;
        result
            .map(|tools| (elapsed_ms, tools))
            .map_err(|error| McpTestFailure { elapsed_ms, error })
    }
    /// Upstream `recordCall`.
    pub async fn record_call(&self, name: &str, elapsed_ms: f64, failed: bool) {
        let Some(connection) = self.connections.get(name) else {
            return;
        };
        let mut entry = connection.entry.lock().await;
        entry.counters.call_count += 1;
        entry.counters.total_latency_ms += elapsed_ms;
        if failed {
            entry.counters.error_count += 1;
        }
    }
    /// Upstream `getLogLines`: the last `max_lines` lines of a server's log ring buffer.
    pub async fn log_lines(&self, name: &str, max_lines: usize) -> Vec<String> {
        let Some(connection) = self.connections.get(name) else {
            return Vec::new();
        };
        let lines = connection
            .entry
            .lock()
            .await
            .logger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_ring_buffer();
        lines[lines.len().saturating_sub(max_lines)..].to_vec()
    }
    /// The names `getServerSnapshots` reports, in snapshot order.
    pub fn server_names(&self) -> Vec<String> {
        self.config
            .as_ref()
            .map(|config| config.servers.keys().cloned().collect())
            .unwrap_or_default()
    }
    pub async fn dispose(&mut self) -> Result<(), McpServiceError> {
        self.binding.release();
        let connections = std::mem::take(&mut self.connections);
        // Mirrors upstream `await Promise.all(entries.map(disposeEntryConnection))`: every dispose runs to completion, first error wins.
        let results =
            futures::future::join_all(connections.values().map(|connection| {
                dispose_entry_connection(connection, &self.registry, self.owner)
            }))
            .await;
        for result in results {
            result?;
        }
        self.output_artifacts.cleanup()?;
        self.config = None;
        self.agent_dir = None;
        self.session_cwd = None;
        self.session_env.clear();
        self.session_mode = None;
        self.deferred.clear();
        self.deferred_registration.clear();
        self.startup_windows.clear();
        self.wire_status_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.wire_status_by_session.clear();
        self.latest_wire_status = Default::default();
        self.pending_auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.interactive_auth_servers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.registration = None;
        self.tier_b_registry = Default::default();
        self.native_tool_search_gate.publish(None);
        self.registration_inputs = None;
        self.self_weak = None;
        Ok(())
    }
}
