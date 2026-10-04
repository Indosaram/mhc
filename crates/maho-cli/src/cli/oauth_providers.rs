//! Port of the CLI half of the OAuth provider lanes (`main.ts` provider assembly).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use maho_ai::auth::types::CredentialStore;
use maho_core::settings_manager::{SettingsManager, SettingsStorage};
use maho_ext_api::Extension;

use super::credentials::AuthStorageCredentialStore;

pub const CLAUDE_SDK_ROOT_ENV: &str = "MAHO_CLAUDE_AGENT_SDK_ROOT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthAssemblyGap {
    pub provider: &'static str,
    pub requirement: String,
}

pub const CURSOR_PROVIDER: &str = "cursor-cli-oauth";

pub fn environment_from_process() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn settings_json(settings: &SettingsManager) -> (serde_json::Value, serde_json::Value) {
    (serde_json::Value::Object(settings.get_global().clone()), serde_json::Value::Object(settings.get_project().clone()))
}

fn resolve_bundled_candidate(candidate: &str) -> Option<PathBuf> {
    let root = std::env::var_os(CLAUDE_SDK_ROOT_ENV)?;
    let path = PathBuf::from(root).join(candidate);
    path.is_file().then_some(path)
}

pub struct OAuthExtensionRequest {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub settings: Arc<dyn Fn() -> maho_ext_anthropic_subscription::settings::ProviderSettings + Send + Sync>,
    pub store: Arc<dyn CredentialStore>,
}

pub fn anthropic_subscription_settings(
    settings: &SettingsManager,
    environment: &BTreeMap<String, String>,
) -> Arc<dyn Fn() -> maho_ext_anthropic_subscription::settings::ProviderSettings + Send + Sync> {
    let (global, project) = settings_json(settings);
    let environment = environment.clone();
    Arc::new(move || maho_ext_anthropic_subscription::settings::load(&global, &project, &environment))
}

pub fn resolve_claude_executable(
    override_path: Option<&std::path::Path>,
) -> std::io::Result<maho_ext_anthropic_subscription::executable::ExecutableResolution> {
    let platform = if cfg!(windows) { "win32" } else { std::env::consts::OS };
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    let path = std::env::var("PATH").ok();
    maho_ext_anthropic_subscription::executable::describe(
        maho_ext_anthropic_subscription::executable::ResolveInput {
            platform,
            arch,
            prefer_musl: false,
            override_path,
            path: path.as_deref(),
        },
        resolve_bundled_candidate,
        None,
        None,
    )
}

pub fn anthropic_subscription_extension(request: OAuthExtensionRequest) -> Result<Box<dyn Extension>, OAuthAssemblyGap> {
    let resolution = resolve_claude_executable(None).map_err(|error| OAuthAssemblyGap {
        provider: "claude-sdk-oauth",
        requirement: format!("Claude executable resolution failed: {error}"),
    })?;
    let executable = resolution.executable.ok_or(OAuthAssemblyGap {
        provider: "claude-sdk-oauth",
        requirement: "no Claude executable resolved (set --claude-path, install `claude` on PATH, or set MAHO_CLAUDE_AGENT_SDK_ROOT for the bundled SDK)".to_owned(),
    })?;
    let oauth = Arc::new(maho_ext_anthropic_subscription::oauth_login::AnthropicSubscriptionOAuth::native(
        request.store,
        Arc::clone(&request.settings),
        executable.clone(),
    ));
    Ok(Box::new(maho_ext_anthropic_subscription::extension::AnthropicSubscriptionExtension::native(
        oauth,
        executable,
        resolution.source,
        request.cwd,
        request.agent_dir,
        request.environment,
    )))
}

pub fn cursor_cli_settings(
    settings: &SettingsManager,
    environment: &BTreeMap<String, String>,
) -> maho_ext_cursor_cli_oauth::settings::CursorCliOauthProviderSettings {
    let (global, project) = settings_json(settings);
    maho_ext_cursor_cli_oauth::settings::resolve_settings(&[global, project], environment)
}

pub fn resolve_cursor_executable(
    environment: &BTreeMap<String, String>,
    settings: &maho_ext_cursor_cli_oauth::settings::CursorCliOauthProviderSettings,
    home: &std::path::Path,
) -> Result<String, maho_ext_cursor_cli_oauth::executable::CursorAgentNotInstalledError> {
    maho_ext_cursor_cli_oauth::executable::resolve_default(environment, settings.executable_path.as_deref(), home)
}

pub struct CursorAssembly {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    pub home: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub store: Arc<dyn CredentialStore>,
    pub storage: Arc<dyn SettingsStorage>,
}

pub fn cursor_cli_extension(assembly: CursorAssembly) -> Result<Box<dyn Extension>, OAuthAssemblyGap> {
    let CursorAssembly { cwd, agent_dir, home, environment, store, storage } = assembly;
    let settings = {
        let loader = Arc::new(Mutex::new(maho_ext_cursor_cli_oauth::settings::SettingsLoader::default()));
        let cwd_text = cwd.to_string_lossy().into_owned();
        let agent_dir_text = agent_dir.to_string_lossy().into_owned();
        let home_text = home.to_string_lossy().into_owned();
        let environment = environment.clone();
        Arc::new(move || loader.lock().unwrap_or_else(PoisonError::into_inner).load(&cwd_text, &agent_dir_text, &home_text, &environment))
    };
    let resolve = maho_ext_cursor_cli_oauth::oauth_login::production_executable_check(environment.clone(), home.clone());
    let persist_acknowledgement = maho_ext_cursor_cli_oauth::oauth_login::production_acknowledgement_writer(Arc::clone(&storage));
    let persist_enabled = maho_ext_cursor_cli_oauth::oauth_login::production_enabled_writer(Arc::clone(&storage));
    let now: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| (maho_ai::utils::diagnostics::now_ms() / 1000.0) as i64);
    let oauth = Arc::new(maho_ext_cursor_cli_oauth::oauth_login::CursorCliOAuth::native(
        store, settings.clone(), resolve, persist_acknowledgement, persist_enabled, now,
    ));
    let resolved = resolve_cursor_executable(&environment, &settings(), &home).map_err(|error| OAuthAssemblyGap {
        provider: CURSOR_PROVIDER,
        requirement: error.to_string(),
    })?;
    Ok(Box::new(maho_ext_cursor_cli_oauth::extension::CursorCliExtension::native(
        oauth,
        PathBuf::from(resolved),
        cwd,
        agent_dir,
        environment,
    )))
}

pub struct OAuthAssembly {
    pub factories: Vec<maho_ext_host::loader::NativeExtensionFactory>,
    pub gaps: Vec<OAuthAssemblyGap>,
}

pub fn oauth_extension_factories(
    request: OAuthExtensionRequest,
    cursor: CursorAssembly,
) -> OAuthAssembly {
    let mut factories = Vec::new();
    let mut gaps = Vec::new();
    match anthropic_subscription_extension(request) {
        Ok(extension) => factories.push(builtin_factory("claude-sdk-oauth", extension)),
        Err(gap) => gaps.push(gap),
    }
    match cursor_cli_extension(cursor) {
        Ok(extension) => factories.push(builtin_factory("cursor-cli-oauth", extension)),
        Err(gap) => gaps.push(gap),
    }
    OAuthAssembly { factories, gaps }
}

fn builtin_factory(id: &str, extension: Box<dyn Extension>) -> maho_ext_host::loader::NativeExtensionFactory {
    let path = format!("<builtin:{id}>");
    maho_ext_host::loader::NativeExtensionFactory {
        path: path.clone(),
        source_info: maho_ext_api::SourceInfo { path, source: "builtin".to_owned(), ..Default::default() },
        extension,
    }
}
