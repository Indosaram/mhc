//! Port of the CLI half of the OAuth provider lanes (`main.ts` provider assembly).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use maho_ai::auth::types::CredentialStore;
use maho_core::settings_manager::{SettingsManager, SettingsStorage};
use maho_ext_api::Extension;

pub const CLAUDE_SDK_ROOT_ENV: &str = "MAHO_CLAUDE_AGENT_SDK_ROOT";

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

/// senpi `defaultExecutableDeps`: `CLAUDE_CODE_EXECUTABLE`, then the bundled SDK, then `claude` on
/// PATH, re-read on every call so a later install or override needs no restart.
pub fn claude_executable_resolver() -> maho_ext_anthropic_subscription::executable::ExecutableResolver {
    Arc::new(|| -> anyhow::Result<(PathBuf, Option<maho_ext_anthropic_subscription::executable::ExecutableSource>)> {
        let override_path = std::env::var_os("CLAUDE_CODE_EXECUTABLE").map(PathBuf::from);
        let resolution = resolve_claude_executable(override_path.as_deref())?;
        match resolution.executable {
            Some(executable) => Ok((executable, resolution.source)),
            None => {
                let platform = if cfg!(windows) { "win32" } else { std::env::consts::OS };
                let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
                let tried = resolution.tried.iter().map(|candidate| format!("  - {candidate}")).collect::<Vec<_>>().join("\n");
                Err(anyhow::anyhow!(
                    "Claude Code executable not found for {platform}-{arch}. Tried:\n{tried}\nReinstall @anthropic-ai/claude-agent-sdk without --omit=optional, install Claude Code so `claude` is on PATH, or set CLAUDE_CODE_EXECUTABLE to the binary."
                ))
            }
        }
    })
}

/// senpi `anthropic-subscription/index.ts`: registration is unconditional; the executable is
/// resolved per turn and per ambient probe.
pub fn anthropic_subscription_extension(request: OAuthExtensionRequest) -> Box<dyn Extension> {
    let resolve = claude_executable_resolver();
    let oauth = Arc::new(maho_ext_anthropic_subscription::oauth_login::AnthropicSubscriptionOAuth::native_with_resolver(
        request.store,
        Arc::clone(&request.settings),
        Arc::clone(&resolve),
    ));
    Box::new(maho_ext_anthropic_subscription::extension::AnthropicSubscriptionExtension::native_with_resolver(
        oauth,
        resolve,
        request.cwd,
        request.agent_dir,
        request.environment,
    ))
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

/// senpi `cursor-cli-oauth/index.ts`: "Registration is unconditional" - a missing cursor-agent only
/// makes `check` and the turn name the install guidance.
pub fn cursor_cli_extension(assembly: CursorAssembly) -> Box<dyn Extension> {
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
    let now: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(|| maho_ai::utils::diagnostics::now_ms() / 1000);
    let oauth = Arc::new(maho_ext_cursor_cli_oauth::oauth_login::CursorCliOAuth::native(
        store, settings.clone(), resolve, persist_acknowledgement, persist_enabled, now,
    ));
    let resolve_path = maho_ext_cursor_cli_oauth::oauth_login::production_executable_resolver(environment.clone(), home);
    Box::new(maho_ext_cursor_cli_oauth::extension::CursorCliExtension::native_with_resolver(
        oauth,
        resolve_path,
        cwd,
        agent_dir,
        environment,
    ))
}

/// The two provider lanes, in senpi's `builtinExtensions` order.
pub fn oauth_extension_factories(
    request: OAuthExtensionRequest,
    cursor: CursorAssembly,
) -> Vec<maho_ext_host::loader::NativeExtensionFactory> {
    vec![
        builtin_factory("claude-sdk-oauth", anthropic_subscription_extension(request)),
        builtin_factory(CURSOR_PROVIDER, cursor_cli_extension(cursor)),
    ]
}

/// The OAuth lanes every runtime mounts (senpi loads them as builtins in every resource loader):
/// built from the launch settings, the shared `auth.json` credential store and the process
/// environment.
pub fn runtime_oauth_factories(settings: &SettingsManager, cwd: &std::path::Path, agent_dir: &str) -> Vec<maho_ext_host::loader::NativeAsyncExtensionFactory> {
    let environment = environment_from_process();
    let store = super::credentials::AuthStorageCredentialStore::create(
        &std::path::Path::new(agent_dir).join("auth.json").to_string_lossy(),
    ).into_shared();
    let cwd_text = cwd.to_string_lossy().into_owned();
    let home = maho_core::config::home_dir();
    super::default_extensions::async_factories(oauth_extension_factories(
        OAuthExtensionRequest {
            cwd: cwd.to_path_buf(), agent_dir: agent_dir.into(), environment: environment.clone(),
            settings: anthropic_subscription_settings(settings, &environment),
            store: Arc::clone(&store),
        },
        CursorAssembly {
            cwd: cwd.to_path_buf(), agent_dir: agent_dir.into(), home: home.clone().into(),
            environment, store,
            storage: Arc::new(maho_core::settings_manager::FileSettingsStorage::new(&cwd_text, agent_dir, &home)),
        },
    ))
}

fn builtin_factory(id: &str, extension: Box<dyn Extension>) -> maho_ext_host::loader::NativeExtensionFactory {
    let path = format!("<builtin:{id}>");
    maho_ext_host::loader::NativeExtensionFactory {
        path: path.clone(),
        source_info: maho_ext_api::SourceInfo { path, source: "builtin".to_owned(), ..Default::default() },
        extension,
    }
}
