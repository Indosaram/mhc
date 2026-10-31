use crate::cli::experimental::command_options::AuthInput;
pub const ENV_RADIUS_GATEWAY: &str = "PI_RADIUS_GATEWAY";
pub async fn explicit_token(input: Option<&AuthInput>, cwd: &str, signal: Option<&maho_ai::utils::abort::AbortSignal>) -> Result<Option<String>, String> {
    if signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { return Err("The operation was aborted".to_owned()); }
    let Some(input) = input else { return Ok(None); };
    let value = match input {
        AuthInput::Token(token) => token.clone(),
        AuthInput::File(path) => {
            let path = crate::utils::paths::resolve_path(path, cwd, &Default::default())?;
            let read = tokio::fs::read_to_string(path);
            if let Some(signal) = signal { tokio::select! { biased; _ = signal.cancelled() => return Err("The operation was aborted".to_owned()), value = read => value.map_err(|error| error.to_string())? } }
            else { read.await.map_err(|error| error.to_string())? }
        }
    };
    let token = value.trim();
    if token.is_empty() { return Err("Radius authentication token must not be empty".to_owned()); }
    Ok(Some(token.to_owned()))
}
/// senpi `RadiusRelayAuth`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RadiusRelayAuth { pub gateway: String, pub token: String }
/// senpi `runtime.getAuth("radius", { minOAuthValidityMs, signal })`, injectable so the stored-credential path is testable.
pub type StoredRadiusAuth = std::sync::Arc<dyn Fn(maho_ai::models::AuthResolutionOverrides) -> futures::future::BoxFuture<'static, Result<Option<maho_ai::models::AuthResolution>, String>> + Send + Sync>;
/// The stored credential is refreshed when it expires within five minutes.
pub const RADIUS_MIN_OAUTH_VALIDITY_MS: u64 = 5 * 60_000;
/// senpi `ModelRuntime.create({ refreshOnCreate: false, allowModelNetwork: false })` + `getAuth("radius")` over the agent dir.
pub fn model_runtime_radius_auth() -> StoredRadiusAuth {
    let runtime = std::sync::Arc::new(tokio::sync::OnceCell::<maho_core::model_runtime::ModelRuntime>::new());
    std::sync::Arc::new(move |overrides| {
        let runtime = runtime.clone();
        Box::pin(async move {
            let runtime = runtime.get_or_try_init(|| async {
                let agent_dir = std::path::PathBuf::from(maho_core::config::get_agent_dir());
                maho_core::model_runtime::ModelRuntime::create(maho_core::model_runtime::CreateModelRuntimeOptions { models_path: Some(agent_dir.join("models.json")), auth_path: Some(agent_dir.join("auth.json")), agent_dir: Some(agent_dir), allow_model_network: Some(false), refresh_on_create: Some(false), ..Default::default() }).await.map_err(|error| error.to_string())
            }).await?;
            runtime.get_auth_with_overrides("radius", &overrides).await.map_err(|error| error.to_string())
        })
    })
}
/// senpi `RadiusRelayAuthResolver`: resolves explicit or stored Radius credentials anew for every relay connection attempt.
pub struct RadiusRelayAuthResolver { input: Option<AuthInput>, gateway: String, cwd: String, offline: bool, stored: StoredRadiusAuth }
impl RadiusRelayAuthResolver {
    /// senpi `new RadiusRelayAuthResolver(input, gateway = PI_RADIUS_GATEWAY ?? DEFAULT_RADIUS_GATEWAY)`, offline from `PI_OFFLINE`.
    pub fn new(input: Option<AuthInput>, cwd: &str) -> Self {
        let gateway = std::env::var(ENV_RADIUS_GATEWAY).unwrap_or_else(|_| maho_ai::providers::radius_config::DEFAULT_RADIUS_GATEWAY.to_owned());
        Self::with_parts(input, &gateway, cwd, std::env::var_os("PI_OFFLINE").is_some(), model_runtime_radius_auth())
    }
    pub fn with_parts(input: Option<AuthInput>, gateway: &str, cwd: &str, offline: bool, stored: StoredRadiusAuth) -> Self {
        Self { input, gateway: maho_ai::providers::radius_config::normalize_radius_gateway_url(gateway), cwd: cwd.to_owned(), offline, stored }
    }
    pub fn gateway(&self) -> &str { &self.gateway }
    /// senpi `resolve({ required, signal })`.
    pub async fn resolve(&self, required: bool, signal: Option<&maho_ai::utils::abort::AbortSignal>) -> Result<Option<RadiusRelayAuth>, String> {
        let aborted = || signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted);
        if aborted() { return Err("The operation was aborted".to_owned()); }
        if self.offline {
            if required { return Err("Radius relay connections are unavailable in offline mode".to_owned()); }
            return Ok(None);
        }
        if let Some(token) = explicit_token(self.input.as_ref(), &self.cwd, signal).await? { return Ok(Some(RadiusRelayAuth { gateway: self.gateway.clone(), token })); }
        let overrides = maho_ai::models::AuthResolutionOverrides { min_oauth_validity_ms: Some(RADIUS_MIN_OAUTH_VALIDITY_MS), signal: signal.cloned(), ..Default::default() };
        let resolution = (self.stored)(overrides).await?;
        if aborted() { return Err("The operation was aborted".to_owned()); }
        if let Some(token) = crate::cli::auth_command::get_auth_credential(resolution.as_ref()).filter(|token| !token.is_empty()) { return Ok(Some(RadiusRelayAuth { gateway: self.gateway.clone(), token: token.to_owned() })); }
        if required { return Err(format!("Radius authentication is required; start {} and run /login radius, then retry", maho_core::config::app_name())); }
        Ok(None)
    }
}
