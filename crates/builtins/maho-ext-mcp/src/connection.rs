pub use crate::connection_types::*;
use crate::{
    config_schema::McpServerConfig,
    errors::{McpError, McpErrorKind},
    log::McpLogger,
    transport::{
        McpTransportConnection, connect_mcp_transport, create_mcp_transport, shutdown_mcp_transport,
    },
    transport_sdk::McpClient,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{broadcast, watch};
type ConnectResult = Option<Result<Arc<McpClient>, McpError>>;
struct ConnectionInner {
    authorization: maho_ext_api::AbortSignal,
    state: ServerConnectionState,
    generation: u64,
    last_error: Option<McpError>,
    transport: Option<Arc<McpTransportConnection>>,
    pending: Option<watch::Receiver<ConnectResult>>,
}
pub struct ServerConnection {
    pub server_name: String,
    config: McpServerConfig,
    env: Option<BTreeMap<String, String>>,
    logger: Arc<Mutex<McpLogger>>,
    inner: Mutex<ConnectionInner>,
    states: broadcast::Sender<ServerConnectionStateChangedEvent>,
    tools: broadcast::Sender<ServerConnectionToolsChangedEvent>,
    project_policy: Mutex<Option<crate::transport::ProjectPolicyResolver>>,
    auth: Mutex<Option<Arc<crate::auth::oauth_refresh::McpRefreshManager>>>,
    auth_challenge_handler: Mutex<Option<Arc<dyn crate::auth::challenge::McpAuthChallengeHandler>>>,
    elicitation_ui: Mutex<Option<Arc<dyn maho_ext_api::ExtensionUi>>>,
    /// Upstream `#elicitationEnabled`: false withholds the elicitation capability from every transport.
    elicitation_enabled: std::sync::atomic::AtomicBool,
    /// Upstream `requestLifetimeSignal`: aborted (and replaced) on renew, bump, disable and dispose, so an
    /// MRTR origin issued for an earlier transport never outlives it.
    request_lifetime: Mutex<maho_ext_api::AbortSignal>,
    /// Upstream `resolveConfig`: a server that declares references re-reads them for every new transport.
    resolve_config: Mutex<Option<crate::config_generation::McpGenerationRenewal>>,
    /// Upstream `#stopFormReadiness`: the private-form readiness subscription of an enforced binding.
    form_readiness: Mutex<Option<crate::form_types::McpFormUnsubscribe>>,
    /// Upstream `elicitationUiProvider` readiness; a pooled connection installs its lease aggregate.
    readiness: Mutex<Option<crate::elicitation::McpFormReadiness>>,
    /// Upstream `#renegotiatingForms` single flight.
    renegotiating_forms: std::sync::atomic::AtomicBool,
}
impl ServerConnection {
    /// The resolved configuration this connection opens transports from.
    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }
    /// Upstream `declarationIdentity`: the project authority's identity, when one admitted this server.
    pub fn declaration_identity(&self) -> Option<String> {
        crate::project_skill_policy::get_project_mcp_authority(&self.config)
            .map(|authority| authority.identity.clone())
    }
    pub fn env(&self) -> Option<&BTreeMap<String, String>> {
        self.env.as_ref()
    }
    pub fn new(
        server: &str,
        config: McpServerConfig,
        env: Option<BTreeMap<String, String>>,
        logger: Arc<Mutex<McpLogger>>,
    ) -> Arc<Self> {
        let (states, _) = broadcast::channel(256);
        let (tools, _) = broadcast::channel(256);
        let state = if config.enabled == Some(true) {
            ServerConnectionState::Idle
        } else {
            ServerConnectionState::Disabled
        };
        Arc::new(Self {
            server_name: server.into(),
            config,
            env,
            logger,
            inner: Mutex::new(ConnectionInner {
                state,
                authorization: maho_ext_api::AbortSignal::default(),
                generation: 0,
                last_error: None,
                transport: None,
                pending: None,
            }),
            states,
            tools,
            project_policy: Mutex::new(None),
            auth: Mutex::new(None),
            auth_challenge_handler: Mutex::new(None),
            elicitation_ui: Mutex::new(None),
            elicitation_enabled: std::sync::atomic::AtomicBool::new(true),
            request_lifetime: Mutex::new(maho_ext_api::AbortSignal::default()),
            resolve_config: Mutex::new(None),
            form_readiness: Mutex::new(None),
            readiness: Mutex::new(None),
            renegotiating_forms: std::sync::atomic::AtomicBool::new(false),
        })
    }
    pub fn set_project_policy(&self, resolve: crate::transport::ProjectPolicyResolver) {
        *self
            .project_policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(resolve);
    }
    pub fn set_auth(&self, refresh: Arc<crate::auth::oauth_refresh::McpRefreshManager>) {
        *self
            .auth
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(refresh);
    }
    pub fn set_auth_challenge_handler(
        &self,
        handler: Arc<dyn crate::auth::challenge::McpAuthChallengeHandler>,
    ) {
        *self
            .auth_challenge_handler
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handler);
    }
    pub fn set_elicitation_ui(&self, ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>) {
        *self
            .elicitation_ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ui;
    }
    /// Install the readiness provider every transport negotiates with (shared-connection.ts aggregate).
    pub fn set_form_readiness(&self, readiness: crate::elicitation::McpFormReadiness) {
        *self
            .readiness
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(readiness);
    }
    fn form_readiness_provider(&self) -> Option<crate::elicitation::McpFormReadiness> {
        self.readiness
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Upstream `#elicitationUiProvider?.enforced?.()`.
    fn forms_enforced(&self) -> bool {
        match self.form_readiness_provider() {
            Some(readiness) => (readiness.enforced)(),
            None => self.private_forms().is_some(),
        }
    }
    pub fn set_elicitation_enabled(&self, enabled: bool) {
        self.elicitation_enabled
            .store(enabled, std::sync::atomic::Ordering::Release);
    }
    pub fn elicitation_enabled(&self) -> bool {
        self.elicitation_enabled
            .load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn elicitation_ui(&self) -> Option<Arc<dyn maho_ext_api::ExtensionUi>> {
        self.elicitation_ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub fn project_policy(&self) -> Option<crate::transport::ProjectPolicyResolver> {
        self.project_policy
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// The live reference renewal, when this server declares references.
    pub fn generation_renewal(&self) -> Option<crate::config_generation::McpGenerationRenewal> {
        self.resolve_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn private_forms(&self) -> Option<crate::form_types::McpFormBinding> {
        self.elicitation_ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(crate::form_types::private_mcp_forms)
    }
    /// Upstream `#legacyFormsChanged`: an enforced legacy client whose advertised forms no longer match the
    /// presenter's readiness.
    fn legacy_forms_changed(&self, client: &McpClient) -> bool {
        if !self.forms_enforced() || crate::modern::request::is_modern_mcp_client(client) {
            return false;
        }
        let ui = self
            .elicitation_ui
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        crate::modern::request::legacy_mcp_forms_advertised(client)
            != Some(crate::elicitation::legacy_mcp_form_eligible_with(
                self.form_readiness_provider().as_ref(),
                ui.as_ref(),
                self.elicitation_enabled(),
            ))
    }
    /// Upstream `#refreshLegacyForms`: one renewal at a time; a failure degrades the connection.
    fn refresh_legacy_forms(self: &Arc<Self>) {
        if self.state() != ServerConnectionState::Connected {
            return;
        }
        let Ok(client) = self.client() else {
            return;
        };
        if !self.legacy_forms_changed(&client)
            || self
                .renegotiating_forms
                .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let connection = self.clone();
        tokio::spawn(async move {
            if let Err(error) = connection.renew().await {
                connection.mark_failure(ServerConnectionState::Degraded, Some(error));
            }
            connection
                .renegotiating_forms
                .store(false, std::sync::atomic::Ordering::Release);
        });
    }
    fn observe_form_readiness(self: &Arc<Self>) {
        let subscribe = match self.form_readiness_provider() {
            Some(readiness) => (readiness.enforced)().then(|| readiness.subscribe.clone()),
            None => self
                .private_forms()
                .and_then(|forms| forms.subscribe.clone()),
        };
        let Some(subscribe) = subscribe else {
            return;
        };
        let mut readiness = self
            .form_readiness
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if readiness.is_none() {
            let weak = Arc::downgrade(self);
            *readiness = Some(subscribe(Arc::new(move || {
                if let Some(connection) = weak.upgrade() {
                    connection.refresh_legacy_forms();
                }
            })));
        }
    }
    fn stop_form_readiness(&self) {
        let stop = self
            .form_readiness
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(stop) = stop {
            stop();
        }
    }
    pub fn request_lifetime_signal(&self) -> maho_ext_api::AbortSignal {
        self.request_lifetime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    fn end_request_lifetime(&self, replace: bool) {
        let mut lifetime = self
            .request_lifetime
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lifetime.abort();
        if replace {
            *lifetime = maho_ext_api::AbortSignal::default();
        }
    }
    pub fn set_resolve_config(
        &self,
        renewal: Option<crate::config_generation::McpGenerationRenewal>,
    ) {
        *self
            .resolve_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = renewal;
    }
    pub fn state(&self) -> ServerConnectionState {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .state
    }
    pub fn generation(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .generation
    }
    pub fn last_error(&self) -> Option<McpError> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_error
            .clone()
    }
    pub fn on_state_change(&self) -> broadcast::Receiver<ServerConnectionStateChangedEvent> {
        self.states.subscribe()
    }
    pub fn on_tools_changed(&self) -> broadcast::Receiver<ServerConnectionToolsChangedEvent> {
        self.tools.subscribe()
    }
    fn error(&self, message: String, phase: &str) -> McpError {
        let mut error = McpError::new(McpErrorKind::Connect, message);
        error.phase = Some(phase.into());
        error.server_name = Some(self.server_name.clone());
        error
    }
    fn transition(
        &self,
        inner: &mut ConnectionInner,
        state: ServerConnectionState,
        error: Option<McpError>,
    ) {
        if inner.state == state {
            return;
        }
        let previous_state = inner.state;
        inner.state = state;
        let _ = self.states.send(ServerConnectionStateChangedEvent {
            server_name: self.server_name.clone(),
            generation: inner.generation,
            state,
            previous_state,
            error,
        });
    }
    pub fn mark_failure(&self, state: ServerConnectionState, error: Option<McpError>) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.last_error = error.clone();
        self.transition(&mut inner, state, error);
    }
    /// Upstream `markToolsChanged(cause = "notification")`.
    pub fn mark_tools_changed(&self) {
        self.emit_tools_changed(ServerConnectionToolsChangedCause::Notification);
    }
    fn emit_tools_changed(&self, cause: ServerConnectionToolsChangedCause) {
        let _ = self.tools.send(ServerConnectionToolsChangedEvent {
            server_name: self.server_name.clone(),
            generation: self.generation(),
            cause,
        });
    }
    pub fn client(&self) -> Result<Arc<McpClient>, McpError> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .transport
            .as_ref()
            .ok_or_else(|| {
                self.error(
                    format!("MCP server {} is not connected", self.server_name),
                    "client",
                )
            })?
            .client()
    }
    pub fn get_root_pid(&self) -> Option<u32> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .transport
            .as_ref()
            .and_then(|transport| transport.get_root_pid())
    }
    pub fn connect(
        self: &Arc<Self>,
    ) -> futures::future::BoxFuture<'static, Result<Arc<McpClient>, McpError>> {
        let this = self.clone();
        Box::pin(async move {
            this.observe_form_readiness();
            let mut receiver = {
                let mut inner = this
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if inner.state == ServerConnectionState::Disabled {
                    return Err(this.error(
                        format!("MCP server {} is disabled", this.server_name),
                        "connect",
                    ));
                }
                if inner.state == ServerConnectionState::Connected {
                    return inner
                        .transport
                        .as_ref()
                        .ok_or_else(|| this.error("connected transport absent".into(), "client"))?
                        .client();
                }
                if let Some(pending) = &inner.pending {
                    pending.clone()
                } else {
                    let (sender, receiver) = watch::channel(None);
                    inner.pending = Some(receiver.clone());
                    let generation = inner.generation;
                    let connection = this.clone();
                    tokio::spawn(async move {
                        let result = connection.open_connection(generation).await;
                        {
                            let mut inner = connection
                                .inner
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if inner.generation == generation {
                                inner.pending = None;
                            }
                        }
                        sender.send_replace(Some(result));
                    });
                    receiver
                }
            };
            loop {
                if let Some(result) = receiver.borrow().clone() {
                    return result;
                }
                if receiver.changed().await.is_err() {
                    return Err(this.error("connect task closed".into(), "connect"));
                }
            }
        })
    }
    async fn open_connection(
        self: &Arc<Self>,
        generation: u64,
    ) -> Result<Arc<McpClient>, McpError> {
        let signal = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .authorization
            .clone();
        let materialized =
            match crate::project_skill_policy::get_project_mcp_authority(&self.config) {
                Some(authority) => {
                    let transport =
                        crate::project_stdio_transport::ProjectStdioTransport::new(authority);
                    let started = transport.start_with_signal(Some(signal)).await?;
                    Some(started)
                }
                None => None,
            };
        // A changed reference snapshot opens no transport: the owner re-keys under the new identity.
        let renewal = self
            .resolve_config
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let renewed = match &renewal {
            Some(renewal) if materialized.is_none() => Some(renewal.resolve_config()?),
            _ => None,
        };
        let (config, env) = materialized.as_ref().map_or(
            (renewed.as_ref().unwrap_or(&self.config), self.env.as_ref()),
            |materialized| (&materialized.config, Some(&materialized.env)),
        );
        let transport =
            match create_mcp_transport(&self.server_name, config, env, self.logger.clone()) {
                Ok(mut transport) => {
                    transport.resolve_project_policy = self
                        .project_policy
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    transport.project_policy =
                        materialized.is_some() || transport.resolve_project_policy.is_some();
                    transport.auth = self
                        .auth
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    transport.auth_challenge_handler = self
                        .auth_challenge_handler
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    transport.elicitation_ui = self
                        .elicitation_ui
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    transport.form_readiness = self.form_readiness_provider();
                    transport.elicitation_enabled = self.elicitation_enabled();
                    Arc::new(transport)
                }
                Err(error) => {
                    let mut inner = self
                        .inner
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if inner.generation == generation
                        && inner.state != ServerConnectionState::Disabled
                    {
                        inner.last_error = Some(error.clone());
                        self.transition(
                            &mut inner,
                            ServerConnectionState::Degraded,
                            Some(error.clone()),
                        );
                    }
                    return Err(error);
                }
            };
        {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.generation != generation || inner.state == ServerConnectionState::Disabled {
                return Err(self.error(
                    format!("MCP server {} connect was superseded", self.server_name),
                    "connect",
                ));
            }
            inner.transport = Some(transport.clone());
            self.transition(&mut inner, ServerConnectionState::Connecting, None);
        }
        let result = match connect_mcp_transport(&transport).await {
            Ok(client) => Ok(client),
            // connection.ts #connectTransport: modern discovery and needs-auth failures pass through undiagnosed.
            Err(error)
                if error.phase.as_deref() == Some("modern-stdio")
                    || crate::needs_auth::is_mcp_needs_auth_error(&error) =>
            {
                Err(error)
            }
            Err(error) => Err(crate::diagnose::diagnose_mcp_connect_failure(
                &self.server_name,
                &self.config,
                self.env.as_ref(),
                &error,
                self.logger.clone(),
            )
            .await),
        };
        let current = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.generation != generation || inner.state == ServerConnectionState::Disabled {
                false
            } else {
                match &result {
                    Ok(_) => {
                        inner.last_error = None;
                        self.transition(&mut inner, ServerConnectionState::Connected, None);
                    }
                    Err(error) => {
                        inner.transport = None;
                        inner.last_error = Some(error.clone());
                        let state = if error.kind == McpErrorKind::Auth {
                            ServerConnectionState::NeedsAuth
                        } else {
                            ServerConnectionState::Degraded
                        };
                        self.transition(&mut inner, state, Some(error.clone()));
                    }
                }
                true
            }
        };
        if !current {
            let _ = shutdown_mcp_transport(&transport).await;
            return Err(self.error(
                format!("MCP server {} connect was superseded", self.server_name),
                "connect",
            ));
        }
        if let Ok(client) = &result {
            // Readiness can change while initialize is in flight; never keep that stale negotiation.
            if self.legacy_forms_changed(client) {
                let renew: futures::future::BoxFuture<'_, Result<Arc<McpClient>, McpError>> =
                    Box::pin(self.renew());
                return renew.await;
            }
            self.emit_tools_changed(ServerConnectionToolsChangedCause::Connect);
            self.subscribe_client(client, generation);
        }
        result
    }
    fn subscribe_client(self: &Arc<Self>, client: &Arc<McpClient>, generation: u64) {
        let mut notifications = client.notifications.subscribe();
        let mut closed = client.closed.subscribe();
        let weak = Arc::downgrade(self);
        let logger = self.logger.clone();
        let threshold = self.config.log_level.and_then(|level| {
            serde_json::to_value(level)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
        });
        tokio::spawn(async move {
            let mut logging =
                crate::logging::McpServerLogging::new(threshold.as_deref(), None, 0.0);
            let started = tokio::time::Instant::now();
            loop {
                let Some(connection) = weak.upgrade() else {
                    return;
                };
                if connection.generation() != generation
                    || connection.state() == ServerConnectionState::Disabled
                {
                    return;
                }
                if *closed.borrow() {
                    let error = connection.error(
                        format!("MCP server {} transport closed", connection.server_name),
                        "close",
                    );
                    connection.mark_failure(ServerConnectionState::Degraded, Some(error));
                    return;
                }
                drop(connection);
                tokio::select! {
                    result=closed.changed()=>{if result.is_err(){return;}}
                    result=notifications.recv()=>{
                        let value=match result {Ok(value)=>value,Err(broadcast::error::RecvError::Lagged(_))=>continue,Err(_)=>return};
                        let Some(connection)=weak.upgrade() else{return;};
                        if connection.generation()!=generation{return;}
                        match crate::notification_schemas::parse_notification(&value) {
                            Some(crate::notification_schemas::McpNotification::ListChanged {..}|crate::notification_schemas::McpNotification::ResourceUpdated {..})=>connection.mark_tools_changed(),
                            Some(crate::notification_schemas::McpNotification::LoggingMessage {level,logger:server_logger,data,..})=>{
                                if let Some((method,text))=logging.message(&level,&data.unwrap_or(serde_json::Value::Null),server_logger.as_deref(),started.elapsed().as_secs_f64()*1000.0) {
                                    let level=match method {crate::logging::LoggerMethod::Debug=>"debug",crate::logging::LoggerMethod::Info=>"info",crate::logging::LoggerMethod::Warn=>"warning",crate::logging::LoggerMethod::Error=>"error"};
                                    let _=logger.lock().unwrap_or_else(std::sync::PoisonError::into_inner).log(level,&text,None,None);
                                }
                            }
                            None=>(),
                        }
                    }
                }
            }
        });
    }
    async fn invalidate(&self, disabled: bool, clear_error: bool) -> Result<(), McpError> {
        let transport = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.authorization.abort();
            inner.authorization = maho_ext_api::AbortSignal::default();
            self.end_request_lifetime(!disabled);
            inner.generation += 1;
            inner.pending = None;
            if clear_error {
                inner.last_error = None;
            }
            self.transition(
                &mut inner,
                if disabled {
                    ServerConnectionState::Disabled
                } else {
                    ServerConnectionState::Idle
                },
                None,
            );
            inner.transport.take()
        };
        if let Some(transport) = transport {
            shutdown_mcp_transport(&transport).await?;
        }
        Ok(())
    }
    pub async fn bump_generation(&self) -> Result<(), McpError> {
        self.invalidate(self.state() == ServerConnectionState::Disabled, true)
            .await
    }
    pub async fn disable(&self) -> Result<(), McpError> {
        self.stop_form_readiness();
        self.invalidate(true, true).await
    }
    pub async fn dispose(&self) -> Result<(), McpError> {
        self.stop_form_readiness();
        self.invalidate(true, false).await
    }
    pub async fn renew(self: &Arc<Self>) -> Result<Arc<McpClient>, McpError> {
        if self.state() == ServerConnectionState::Disabled {
            return Err(self.error(
                format!("MCP server {} is disabled", self.server_name),
                "renew",
            ));
        }
        self.invalidate(false, false).await?;
        self.connect().await
    }
}
