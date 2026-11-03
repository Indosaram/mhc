use crate::{
    catalog_cache::{McpCachedServerCatalog, write_mcp_cached_server},
    connection::ServerConnection,
    errors::{McpError, McpErrorKind},
    transport_sdk::McpClient,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
/// What one owner's lease brings to the pool (upstream lease `options`): its UI and readiness, whether it
/// may negotiate elicitation, and its project policy.
#[derive(Clone)]
pub struct SharedLeaseOptions {
    pub elicitation_ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
    pub elicitation_enabled: bool,
    pub project_policy: Option<crate::transport::ProjectPolicyResolver>,
}
impl Default for SharedLeaseOptions {
    fn default() -> Self {
        Self {
            elicitation_ui: None,
            elicitation_enabled: true,
            project_policy: None,
        }
    }
}
impl SharedLeaseOptions {
    /// The lease provider's `enforced`: its UI carries a host-issued private form binding.
    fn enforced(&self) -> bool {
        self.elicitation_ui
            .as_ref()
            .and_then(crate::form_types::private_mcp_forms)
            .is_some()
    }
    /// shared-connection.ts `eligible(lease)`: elicitation enabled and the lease's own readiness not false.
    fn eligible(&self) -> bool {
        self.elicitation_enabled
            && crate::elicitation::legacy_mcp_form_eligible(self.elicitation_ui.as_ref(), true)
    }
}
type ReadinessListener = Arc<dyn Fn() + Send + Sync>;
/// The pooled connection's `projectPolicy.resolve`: whichever live lease currently carries a policy.
fn resolve_active_policy(
    shared: &std::sync::Weak<SharedMcpConnection>,
) -> futures::future::BoxFuture<
    'static,
    Result<crate::transport::ProjectMcpMaterialization, McpError>,
> {
    let policy = shared
        .upgrade()
        .and_then(|shared| shared.active_project_policy());
    Box::pin(async move {
        match policy {
            Some(resolve) => resolve().await,
            None => Err(McpError::new(
                McpErrorKind::Connect,
                "Project MCP policy unavailable",
            )),
        }
    })
}
struct SharedState {
    leases: BTreeMap<u64, usize>,
    calls: BTreeMap<u64, usize>,
    disposed: bool,
    in_flight: usize,
    idle: Option<tokio::task::JoinHandle<()>>,
}
pub struct SharedMcpConnection {
    pub connection: Arc<ServerConnection>,
    state: Mutex<SharedState>,
    catalog: tokio::sync::Mutex<Option<(u64, McpCachedServerCatalog)>>,
    renewal: tokio::sync::Mutex<()>,
    agent_dir: PathBuf,
    config_hash: String,
    request_timeout: Duration,
    subscription: Mutex<Option<tokio::task::JoinHandle<()>>>,
    idle_timeout: Duration,
    /// Each owner's re-key, called when a renewal finds the reference snapshot changed (upstream lease
    /// `options.onGenerationChanged`): one owner's renewal re-keys every lease, not only the one that admitted it.
    generation_listeners: Mutex<BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>>,
    /// Each attached owner's lease options, in attach order (upstream `leases` Set order).
    lease_options: Mutex<Vec<(u64, SharedLeaseOptions)>>,
    /// Upstream `#readinessListeners`: the connection's form-readiness observers.
    readiness_listeners: Mutex<Vec<(u64, ReadinessListener)>>,
    next_readiness_listener: std::sync::atomic::AtomicU64,
    /// Upstream `#readinessSubscriptions`: each owner's subscription to its own presenter readiness.
    readiness_subscriptions: Mutex<BTreeMap<u64, crate::form_types::McpFormUnsubscribe>>,
    /// Upstream constructor `options.projectPolicy`: the creating owner's policy, the fallback once no
    /// attached lease carries one.
    original_project_policy: Mutex<Option<crate::transport::ProjectPolicyResolver>>,
}
impl SharedMcpConnection {
    /// Record `owner`'s lease options. The first policy-bearing owner installs the aggregate resolver on the
    /// connection; a presenter's readiness changes fan out to the connection's observers.
    pub fn set_owner_options(self: &Arc<Self>, owner: u64, options: SharedLeaseOptions) {
        let subscribe = options
            .elicitation_ui
            .as_ref()
            .and_then(crate::form_types::private_mcp_forms)
            .and_then(|binding| binding.subscribe);
        let install_policy = options.project_policy.as_ref().is_some_and(|policy| {
            let mut original = self
                .original_project_policy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first = original.is_none();
            if first {
                *original = Some(policy.clone());
            }
            first
        });
        {
            let mut lease_options = self
                .lease_options
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match lease_options.iter_mut().find(|(id, _)| *id == owner) {
                Some((_, existing)) => *existing = options,
                None => lease_options.push((owner, options)),
            }
        }
        if install_policy {
            let weak = Arc::downgrade(self);
            self.connection
                .set_project_policy(Arc::new(move || resolve_active_policy(&weak)));
        }
        let previous = match subscribe {
            Some(subscribe) => {
                let weak = Arc::downgrade(self);
                let unsubscribe = subscribe(Arc::new(move || {
                    if let Some(shared) = weak.upgrade() {
                        shared.notify_readiness();
                    }
                }));
                self.readiness_subscriptions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(owner, unsubscribe)
            }
            None => self
                .readiness_subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&owner),
        };
        if let Some(previous) = previous {
            previous();
        }
        self.notify_readiness();
    }
    /// shared-connection.ts `projectPolicy.resolve`: pool identity guarantees equal authority, but the
    /// original presentation may have closed while another lease remains. Owners with a call in flight come
    /// first (by owner id; upstream keeps call insertion order), then attached leases in attach order.
    pub fn active_project_policy(&self) -> Option<crate::transport::ProjectPolicyResolver> {
        let calls = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .calls
            .keys()
            .copied()
            .collect::<Vec<_>>();
        let active = {
            let options = self
                .lease_options
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let policy_of = |owner: u64| {
                options
                    .iter()
                    .find(|(id, _)| *id == owner)
                    .and_then(|(_, options)| options.project_policy.clone())
            };
            calls.into_iter().find_map(policy_of).or_else(|| {
                options
                    .iter()
                    .find_map(|(_, options)| options.project_policy.clone())
            })
        };
        active.or_else(|| {
            self.original_project_policy
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
    }
    /// The aggregate provider's `enforced`: any lease enforces private forms.
    pub fn readiness_enforced(&self) -> bool {
        self.lease_options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|(_, options)| options.enforced())
    }
    /// The aggregate provider's `eligible`: any ready owner permits negotiation when one enforces;
    /// otherwise every lease must be eligible. Per-operation authority still selects the holder.
    pub fn readiness_eligible(&self) -> bool {
        let options = self
            .lease_options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if options.iter().any(|(_, options)| options.enforced()) {
            return options.iter().any(|(_, options)| options.eligible());
        }
        !options.is_empty() && options.iter().all(|(_, options)| options.eligible())
    }
    /// The aggregate provider's `subscribe`.
    pub fn subscribe_readiness(
        self: &Arc<Self>,
        listener: ReadinessListener,
    ) -> crate::form_types::McpFormUnsubscribe {
        let id = self
            .next_readiness_listener
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.readiness_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, listener));
        let weak = Arc::downgrade(self);
        Box::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared
                    .readiness_listeners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retain(|(listener, _)| *listener != id);
            }
        })
    }
    fn notify_readiness(&self) {
        let listeners = self
            .readiness_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect::<Vec<_>>();
        for listener in listeners {
            listener();
        }
    }
    /// Registers `owner`'s re-key for a changed reference snapshot; it lives until the owner's last release.
    pub fn on_generation_changed(&self, owner: u64, rekey: Arc<dyn Fn() + Send + Sync>) {
        self.generation_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(owner, rekey);
    }
    /// Upstream `#renewedConfig` on `McpConfigGenerationChangedError`: every current lease re-keys.
    pub fn notify_generation_changed(&self) {
        let listeners = self
            .generation_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for rekey in listeners {
            rekey();
        }
    }
    pub fn new(
        connection: Arc<ServerConnection>,
        agent_dir: PathBuf,
        config_hash: String,
        request_timeout: Duration,
    ) -> Arc<Self> {
        Self::with_idle_timeout(
            connection,
            agent_dir,
            config_hash,
            request_timeout,
            Duration::from_secs(600),
        )
    }
    pub fn with_idle_timeout(
        connection: Arc<ServerConnection>,
        agent_dir: PathBuf,
        config_hash: String,
        request_timeout: Duration,
        idle_timeout: Duration,
    ) -> Arc<Self> {
        let mut changes = connection.on_tools_changed();
        let shared = Arc::new(Self {
            connection,
            state: Mutex::new(SharedState {
                leases: BTreeMap::new(),
                calls: BTreeMap::new(),
                disposed: false,
                in_flight: 0,
                idle: None,
            }),
            catalog: tokio::sync::Mutex::new(None),
            renewal: tokio::sync::Mutex::new(()),
            agent_dir,
            config_hash,
            request_timeout,
            subscription: Mutex::new(None),
            idle_timeout,
            generation_listeners: Mutex::new(BTreeMap::new()),
            lease_options: Mutex::new(Vec::new()),
            readiness_listeners: Mutex::new(Vec::new()),
            next_readiness_listener: std::sync::atomic::AtomicU64::new(0),
            readiness_subscriptions: Mutex::new(BTreeMap::new()),
            original_project_policy: Mutex::new(None),
        });
        // shared-connection.ts: the physical connection negotiates with the aggregate over every lease.
        let readiness = Arc::downgrade(&shared);
        shared
            .connection
            .set_form_readiness(crate::elicitation::McpFormReadiness {
                enforced: Arc::new({
                    let readiness = readiness.clone();
                    move || {
                        readiness
                            .upgrade()
                            .is_some_and(|shared| shared.readiness_enforced())
                    }
                }),
                eligible: Arc::new({
                    let readiness = readiness.clone();
                    move || {
                        readiness
                            .upgrade()
                            .is_some_and(|shared| shared.readiness_eligible())
                    }
                }),
                subscribe: Arc::new(move |listener| match readiness.upgrade() {
                    Some(shared) => shared.subscribe_readiness(listener),
                    None => Box::new(|| {}),
                }),
            });
        let weak = Arc::downgrade(&shared);
        let task = tokio::spawn(async move {
            loop {
                match changes.recv().await {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let Some(shared) = weak.upgrade() else {
                            return;
                        };
                        *shared.catalog.lock().await = None;
                    }
                    Err(_) => return,
                }
            }
        });
        *shared
            .subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
        shared
    }
    pub fn attach(
        self: &Arc<Self>,
        owner: u64,
        key: String,
    ) -> Result<Arc<crate::shared_lease::SharedMcpLease>, McpError> {
        self.attach_with_options(owner, key, None)
    }

    pub fn attach_with_options(
        self: &Arc<Self>,
        owner: u64,
        key: String,
        options: Option<SharedLeaseOptions>,
    ) -> Result<Arc<crate::shared_lease::SharedMcpLease>, McpError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed {
            return Err(McpError::new(
                McpErrorKind::Connect,
                "MCP shared connection disposed",
            ));
        }
        if let Some(timer) = state.idle.take() {
            timer.abort();
        }
        *state.leases.entry(owner).or_default() += 1;
        drop(state);
        match options {
            Some(options) => {
                self.set_owner_options(owner, options);
            }
            None => {
                {
                    let mut options = self
                        .lease_options
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !options.iter().any(|(id, _)| *id == owner) {
                        options.push((owner, SharedLeaseOptions::default()));
                    }
                }
                self.notify_readiness();
            }
        }
        Ok(crate::shared_lease::SharedMcpLease::new(
            self.clone(),
            owner,
            key,
        ))
    }
    pub fn release(self: &Arc<Self>, owner: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut unsubscribe = None;
        if let Some(count) = state.leases.get_mut(&owner) {
            *count -= 1;
            if *count == 0 {
                state.leases.remove(&owner);
                self.generation_listeners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&owner);
                self.lease_options
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .retain(|(id, _)| *id != owner);
                unsubscribe = self
                    .readiness_subscriptions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&owner);
            }
        }
        let remaining = !state.leases.is_empty();
        drop(state);
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        if remaining {
            self.notify_readiness();
        }
        self.arm_idle();
    }
    fn arm_idle(self: &Arc<Self>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.disposed || !state.leases.is_empty() || state.in_flight > 0 || state.idle.is_some()
        {
            return;
        }
        let deadline = tokio::time::Instant::now() + self.idle_timeout;
        let weak = Arc::downgrade(self);
        state.idle = Some(tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            let Some(shared) = weak.upgrade() else {
                return;
            };
            let dispose = {
                let mut state = shared
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.idle = None;
                let dispose = !state.disposed && state.leases.is_empty() && state.in_flight == 0;
                if dispose {
                    state.disposed = true;
                }
                dispose
            };
            if dispose {
                if let Some(task) = shared
                    .subscription
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    task.abort();
                }
                if let Err(error) = shared.connection.dispose().await {
                    maho_ext_api::diagnostic(&format!("MCP shared idle disposal failed: {error}"));
                }
            }
        }));
    }
    pub async fn run_request<T>(self: &Arc<Self>, call: impl std::future::Future<Output = T>) -> T {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.in_flight += 1;
            if let Some(timer) = state.idle.take() {
                timer.abort();
            }
        }
        struct RequestGuard(Arc<SharedMcpConnection>);
        impl Drop for RequestGuard {
            fn drop(&mut self) {
                {
                    let mut state = self
                        .0
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.in_flight -= 1;
                }
                self.0.arm_idle();
            }
        }
        let _guard = RequestGuard(self.clone());
        call.await
    }
    pub fn lease_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .leases
            .values()
            .sum()
    }
    pub fn is_disposed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .disposed
    }
    pub fn owner_attached(&self, owner: u64) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        !state.disposed && state.leases.contains_key(&owner)
    }
    pub fn elicitation_owner(&self) -> Option<u64> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.calls.len() != 1 {
            return None;
        }
        state
            .calls
            .keys()
            .next()
            .copied()
            .filter(|owner| state.leases.contains_key(owner))
    }
    pub async fn connect(&self) -> Result<Arc<McpClient>, McpError> {
        let _renewal = self.renewal.lock().await;
        if self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .disposed
        {
            return Err(McpError::new(
                McpErrorKind::Connect,
                "MCP shared connection disposed",
            ));
        }
        if matches!(
            self.connection.state(),
            crate::connection::ServerConnectionState::Connected
                | crate::connection::ServerConnectionState::Connecting
                | crate::connection::ServerConnectionState::Idle
        ) {
            self.connection.connect().await
        } else {
            self.connection.renew().await
        }
    }
    pub async fn call_tool<T>(
        self: &Arc<Self>,
        owner: u64,
        call: impl std::future::Future<Output = T>,
    ) -> T {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *state.calls.entry(owner).or_default() += 1;
        }
        struct CallGuard {
            shared: Arc<SharedMcpConnection>,
            owner: u64,
        }
        impl Drop for CallGuard {
            fn drop(&mut self) {
                let mut state = self
                    .shared
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(count) = state.calls.get_mut(&self.owner) {
                    *count -= 1;
                    if *count == 0 {
                        state.calls.remove(&self.owner);
                    }
                }
            }
        }
        let _guard = CallGuard {
            shared: self.clone(),
            owner,
        };
        self.run_request(call).await
    }
    pub async fn catalog(&self, refresh: bool) -> Result<McpCachedServerCatalog, McpError> {
        let client = self.connect().await?;
        let mut cached = self.catalog.lock().await;
        let generation = self.connection.generation();
        if !refresh
            && let Some((previous, catalog)) = &*cached
            && *previous == generation
        {
            return Ok(catalog.clone());
        }
        let catalog = crate::catalog_cache::collect_server_catalog_for_cache_in(
            &self.connection,
            self.request_timeout,
            &self.config_hash,
            Some(&self.agent_dir),
        )
        .await?;
        if self.connection.generation() == generation {
            if cached
                .as_ref()
                .is_none_or(|(previous, _)| *previous != generation)
            {
                write_mcp_cached_server(
                    &self.agent_dir,
                    &self.connection.server_name,
                    catalog.clone(),
                )
                .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
            }
            *cached = Some((generation, catalog.clone()));
            crate::resources::ensure_mcp_resource_subscriptions(
                client.clone(),
                &catalog.resources,
                self.request_timeout,
            )
            .await;
        }
        Ok(catalog)
    }
    pub async fn invalidate(&self) -> Result<(), McpError> {
        self.connection.bump_generation().await?;
        *self.catalog.lock().await = None;
        Ok(())
    }
    pub async fn dispose(&self) -> Result<(), McpError> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.disposed {
                return Ok(());
            }
            state.disposed = true;
            state.leases.clear();
            if let Some(timer) = state.idle.take() {
                timer.abort();
            }
        }
        self.lease_options
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let subscriptions = std::mem::take(
            &mut *self
                .readiness_subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for unsubscribe in subscriptions.into_values() {
            unsubscribe();
        }
        if let Some(task) = self
            .subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
        self.connection.dispose().await
    }
}
impl Drop for SharedMcpConnection {
    fn drop(&mut self) {
        if let Some(task) = self
            .subscription
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}
