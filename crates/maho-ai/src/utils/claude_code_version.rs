use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;
use std::sync::{Arc, LazyLock, Mutex};

pub const CLAUDE_CODE_VERSION_PIN_ENV: &str = "PI_CLAUDE_CODE_VERSION";
const RELEASE_CHANNEL_URL: &str = "https://storage.googleapis.com/claude-code-dist-86c565f3-f756-42ad-8dfa-d59b1c096819/claude-code-releases/latest";
const NPM_DIST_TAG_URL: &str = "https://registry.npmjs.org/@anthropic-ai/claude-code/latest";

#[derive(Clone)]
pub struct CachedClaudeCodeVersion {
    pub version: String,
    pub checked_at: i64,
}

pub trait ClaudeCodeVersionStore: Send + Sync {
    fn load(&self) -> Option<CachedClaudeCodeVersion>;
    fn save(&self, record: CachedClaudeCodeVersion);
}

#[derive(Default)]
struct Resolver {
    store: Option<Arc<dyn ClaudeCodeVersionStore>>,
    offline: bool,
    loaded: bool,
    version: Option<String>,
    checked_at: i64,
    inflight: bool,
}

static RESOLVER: LazyLock<Mutex<Resolver>> = LazyLock::new(|| Mutex::new(Resolver::default()));
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn semver(value: &str) -> Option<[u64; 3]> {
    let mut parts = value.split('.');
    let mut result = [0; 3];
    for part in &mut result {
        let text = parts.next()?;
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        *part = text.parse().ok()?;
    }
    parts.next().is_none().then_some(result)
}

fn pin(env: Option<&ProviderEnv>) -> Option<String> {
    get_provider_env_value(CLAUDE_CODE_VERSION_PIN_ENV, env)
        .map(|value| value.trim().to_owned())
        .filter(|value| semver(value).is_some())
}

fn current(resolver: &mut Resolver, floor: &str) -> String {
    if !resolver.loaded {
        resolver.loaded = true;
        if let Some(record) = resolver.store.as_ref().and_then(|store| store.load())
            && semver(&record.version).is_some()
        {
            resolver.version = Some(record.version);
            resolver.checked_at = record.checked_at;
        }
    }
    resolver
        .version
        .as_ref()
        .filter(|version| semver(version) > semver(floor))
        .map_or_else(|| floor.to_owned(), Clone::clone)
}

pub fn install_claude_code_version_store(
    store: Option<Arc<dyn ClaudeCodeVersionStore>>,
    offline: bool,
) {
    *RESOLVER.lock().unwrap_or_else(|p| p.into_inner()) = Resolver {
        store,
        offline,
        ..Default::default()
    };
}

async fn fetch_version(client: &reqwest::Client, url: &str, npm: bool) -> Option<String> {
    let lookup = async {
        let response = client
            .get(url)
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let response = response
            .error_for_status()
            .map_err(|error| error.to_string())?;
        let text = response.text().await.map_err(|error| error.to_string())?;
        let text = if npm {
            let data: serde_json::Value =
                serde_json::from_str(&text).map_err(|error| error.to_string())?;
            data["version"].as_str().unwrap_or_default().to_owned()
        } else {
            text
        };
        let version = text
            .split(|ch: char| !ch.is_ascii_digit() && ch != '.')
            .find(|part| semver(part).is_some())
            .map(str::to_owned);
        Ok::<_, String>(version)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), lookup).await {
        Ok(Ok(version)) => version,
        Ok(Err(error)) => {
            eprintln!("Claude Code version lookup failed: {error}");
            None
        }
        Err(error) => {
            eprintln!("Claude Code version lookup timed out: {error}");
            None
        }
    }
}

async fn refresh() {
    let previous_check = RESOLVER
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .checked_at;
    let _refresh = REFRESH.lock().await;
    if RESOLVER
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .checked_at
        != previous_check
    {
        return;
    }
    let client = reqwest::Client::new();
    let (channel, npm) = tokio::join!(
        fetch_version(&client, RELEASE_CHANNEL_URL, false),
        fetch_version(&client, NPM_DIST_TAG_URL, true)
    );
    let latest = [channel, npm]
        .into_iter()
        .flatten()
        .max_by_key(|value| semver(value));
    let mut resolver = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    resolver.checked_at = crate::utils::diagnostics::now_ms();
    if let Some(latest) = latest
        && resolver
            .version
            .as_ref()
            .is_none_or(|old| semver(&latest) > semver(old))
    {
        resolver.version = Some(latest);
    }
    if let (Some(store), Some(version)) = (&resolver.store, &resolver.version) {
        store.save(CachedClaudeCodeVersion {
            version: version.clone(),
            checked_at: resolver.checked_at,
        });
    }
    resolver.inflight = false;
}

pub fn get_claude_code_version(floor: &str, env: Option<&ProviderEnv>) -> String {
    if let Some(pin) = pin(env) {
        return pin;
    }
    let mut resolver = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    let version = current(&mut resolver, floor);
    if resolver.store.is_some()
        && !resolver.offline
        && !resolver.inflight
        && crate::utils::diagnostics::now_ms().saturating_sub(resolver.checked_at)
            >= 6 * 60 * 60 * 1000
    {
        resolver.inflight = true;
        tokio::spawn(refresh());
    }
    version
}

pub fn is_claude_code_version_too_old_error(status: Option<u16>, message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    status == Some(400)
        && (lower.contains("claude_code_version_too_old")
            || (lower.contains("does not support this model; version ")
                && lower.contains(" or newer")))
}

pub async fn recover_claude_code_version(
    message: &str,
    floor: &str,
    advertised: &str,
    env: Option<&ProviderEnv>,
) -> Option<String> {
    if pin(env).is_some() {
        return None;
    }
    let lower = message.to_ascii_lowercase();
    let required = lower.split("version ").skip(1).find_map(|tail| {
        let (version, _) = tail.split_once(" or newer")?;
        semver(version).map(|_| version.to_owned())
    });
    if required.is_none() {
        let enabled = {
            let resolver = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
            resolver.store.is_some() && !resolver.offline
        };
        if enabled {
            refresh().await;
        }
    }
    let mut resolver = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    let before = current(&mut resolver, floor);
    if let Some(required) = required
        && semver(&required) > semver(&before)
    {
        resolver.version = Some(required);
        if let (Some(store), Some(version)) = (&resolver.store, &resolver.version) {
            store.save(CachedClaudeCodeVersion {
                version: version.clone(),
                checked_at: resolver.checked_at,
            });
        }
    }
    let next = current(&mut resolver, floor);
    (semver(&next) > semver(advertised)).then_some(next)
}

pub fn claude_code_version_too_old_hint(advertised: &str) -> String {
    format!(
        "senpi advertised claude-cli/{advertised}; Anthropic requires a newer Claude Code for this model. senpi refreshes the version automatically when online; set {CLAUDE_CODE_VERSION_PIN_ENV}=X.Y.Z to advertise a specific one."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn pin_wins_over_required_version_when_recovering() {
        let env = ProviderEnv::from([(CLAUDE_CODE_VERSION_PIN_ENV.into(), "2.1.1".into())]);
        assert_eq!(get_claude_code_version("2.1.296", Some(&env)), "2.1.1");
        assert_eq!(
            recover_claude_code_version("version 9.0.0 or newer", "2.1.296", "2.1.1", Some(&env))
                .await,
            None
        );
    }
}
