//! Port of senpi `packages/coding-agent/src/core/brand.ts`.
//!
//! Brand profile resolution. A distribution that repackages this engine injects a single JSON
//! environment variable describing how the product presents itself. The engine parses it once at
//! startup and then REMOVES it from the environment, so nested processes spawned by tools inherit a
//! clean environment and keep the engine's own identity.
//!
//! Absent or malformed input leaves every brand-derived value at its standalone default, so a plain
//! install behaves exactly as it did before this module existed.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

/// Environment variable carrying the JSON brand profile (`BRAND_ENV_VAR`).
pub const BRAND_ENV_VAR: &str = "MAHO_BRAND";

/// Where a repackaging distribution publishes itself. Without this the engine keeps checking its
/// own package, which would advertise an update the branded product cannot install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrandUpdateChannel {
    /// Registry package that ships the branded product, e.g. `omo-ai`.
    pub package_name: String,
    /// Dist-tag the product publishes on, e.g. `beta`.
    pub dist_tag: String,
    /// Command shown to the user, e.g. `npm i -g omo-ai@beta`.
    pub command: String,
    /// Release notes URL; `{version}` is replaced with the available version.
    pub changelog_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrandChangelog {
    pub path: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrandProfile {
    /// Product name shown to users and to the model.
    pub name: String,
    /// Executable name used in shell-command contexts when it differs from the display name.
    pub command: Option<String>,
    /// Version shown in the header, terminal titles and `--version`.
    pub display_version: Option<String>,
    /// Config directory name, e.g. `.maho`.
    pub config_dir: String,
    /// When true the agent state lives directly under the config directory, with no `agent` segment.
    pub flat_layout: bool,
    /// Prefix for the product's environment variables, e.g. `MAHO`.
    pub env_prefix: String,
    /// Product token used in the outgoing user agent.
    pub user_agent: String,
    /// Product token used as the provider-side originator, when the distribution overrides it.
    pub originator: Option<String>,
    /// Update channel of the branded product; absent means the product manages updates itself.
    pub update: Option<BrandUpdateChannel>,
    pub changelog: Option<BrandChangelog>,
}

/// The engine's own identity, used when no brand profile is injected: `maho` / `.maho` / `mhc`
/// (plan IS-9, D-M7).
pub fn standalone_brand_profile() -> BrandProfile {
    BrandProfile {
        name: "maho".to_owned(),
        command: Some("mhc".to_owned()),
        display_version: None,
        config_dir: ".maho".to_owned(),
        flat_layout: false,
        env_prefix: "MAHO".to_owned(),
        user_agent: "maho".to_owned(),
        originator: None,
        update: None,
        changelog: None,
    }
}

fn read_string(source: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    let value = source.get(key)?.as_str()?;
    let trimmed = value.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
}

fn read_changelog(source: &serde_json::Map<String, Value>) -> Option<BrandChangelog> {
    let changelog = source.get("changelog")?.as_object()?;
    let path = read_string(changelog, "path")?;
    if !path.starts_with('/') || path.contains('\0') {
        return None;
    }
    Some(BrandChangelog { path, version: read_string(changelog, "version") })
}

fn read_update_channel(source: &serde_json::Map<String, Value>) -> Option<BrandUpdateChannel> {
    let channel = source.get("update")?.as_object()?;
    let package_name = read_string(channel, "packageName")?;
    let command = read_string(channel, "command")?;
    Some(BrandUpdateChannel {
        package_name,
        dist_tag: read_string(channel, "distTag").unwrap_or_else(|| "latest".to_owned()),
        command,
        changelog_url: read_string(channel, "changelogUrl"),
    })
}

/// A config directory names ONE entry inside the home directory. Anything carrying a separator or a
/// parent reference would move agent state - and the migration that copies into it - somewhere the
/// user never agreed to, so such a profile is rejected rather than sanitised.
fn is_safe_config_dir_name(value: &str) -> bool {
    value != "." && value != ".." && !value.contains('/') && !value.contains('\\')
}

/// Parses a brand profile. Returns `None` for absent, malformed, or nameless input; a malformed
/// profile is reported once on stderr and never panics, because a broken brand must not stop the
/// agent from starting.
pub fn parse_brand_profile(raw: Option<&str>) -> Option<BrandProfile> {
    let raw = raw?;
    if raw.trim().is_empty() {
        return None;
    }

    let parsed: Value = match serde_json::from_str(raw) {
        Ok(parsed) => parsed,
        Err(_) => {
            eprintln!("warning: ignoring malformed {BRAND_ENV_VAR} (expected JSON)");
            return None;
        }
    };

    let Some(source) = parsed.as_object() else {
        eprintln!("warning: ignoring malformed {BRAND_ENV_VAR} (expected a JSON object)");
        return None;
    };

    let Some(name) = read_string(source, "name") else {
        eprintln!("warning: ignoring {BRAND_ENV_VAR} without a \"name\"");
        return None;
    };

    let config_dir = read_string(source, "configDir").unwrap_or_else(|| format!(".{name}"));
    if !is_safe_config_dir_name(&config_dir) {
        eprintln!("warning: ignoring {BRAND_ENV_VAR} with an unsafe \"configDir\"");
        return None;
    }

    Some(BrandProfile {
        name: name.clone(),
        command: read_string(source, "command"),
        display_version: read_string(source, "displayVersion"),
        config_dir,
        flat_layout: source.get("flatLayout").and_then(Value::as_bool) == Some(true),
        env_prefix: read_string(source, "envPrefix").unwrap_or_else(|| name.clone()).to_uppercase(),
        user_agent: read_string(source, "userAgent").unwrap_or_else(|| name.clone()),
        originator: read_string(source, "originator"),
        update: read_update_channel(source),
        changelog: read_changelog(source),
    })
}

/// Parses the brand profile without touching the environment (`consumeBrandProfile`).
pub fn consume_brand_profile(env: &HashMap<String, String>) -> Option<BrandProfile> {
    parse_brand_profile(env.get(BRAND_ENV_VAR).map(String::as_str))
}

/// Removes the brand variable so tools spawning the engine again inherit a clean environment and
/// keep the engine's own identity (`scrubBrandFromEnvironment`).
pub fn scrub_brand_from_environment(env: &mut HashMap<String, String>) {
    env.remove(BRAND_ENV_VAR);
}

/// Environment prefixes read after the brand's own prefix, so a machine configured before the
/// rebrand keeps working unchanged.
pub const LEGACY_ENV_PREFIXES: [&str; 3] = ["MAHO", "SENPI", "PI"];

static CACHED_PROFILE: Mutex<Option<Option<BrandProfile>>> = Mutex::new(None);

/// The active brand profile, resolved (and scrubbed from the environment) on first use.
///
/// With no injected profile the engine's own standalone identity is returned, matching senpi's
/// `BRAND?.name || piConfigName || "pi"` fallbacks with maho as the built-in product.
pub fn brand_profile() -> BrandProfile {
    let mut cached = CACHED_PROFILE.lock().expect("brand profile lock");
    if let Some(profile) = cached.as_ref() {
        return profile.clone().unwrap_or_else(standalone_brand_profile);
    }
    let profile = consume_brand_profile(&current_env());
    *cached = Some(profile);
    let resolved = cached.as_ref().and_then(Clone::clone);
    resolved.unwrap_or_else(standalone_brand_profile)
}

/// The raw injected profile, or `None` when the engine runs standalone (`brandProfile()` in TS).
pub fn injected_brand_profile() -> Option<BrandProfile> {
    let mut cached = CACHED_PROFILE.lock().expect("brand profile lock");
    if let Some(profile) = cached.as_ref() {
        return profile.clone();
    }
    let profile = consume_brand_profile(&current_env());
    *cached = Some(profile.clone());
    profile
}

/// exported for tests only (`resetBrandProfileForTests`)
pub fn reset_brand_profile_for_tests() {
    *CACHED_PROFILE.lock().expect("brand profile lock") = None;
}

/// Environment variable names for one setting, most specific first (`brandEnvNames`).
pub fn brand_env_names(suffix: &str, profile: Option<&BrandProfile>) -> Vec<String> {
    let mut prefixes: Vec<String> = Vec::new();
    if let Some(prefix) = profile.map(|p| p.env_prefix.as_str()).filter(|p| !p.is_empty()) {
        prefixes.push(prefix.to_owned());
    }
    for prefix in LEGACY_ENV_PREFIXES {
        prefixes.push(prefix.to_owned());
    }
    let mut names = Vec::new();
    for prefix in prefixes {
        let name = format!("{prefix}_{suffix}");
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Reads one setting across the brand's prefix and the legacy prefixes, returning the first
/// DEFINED value so that an explicitly empty value keeps its existing meaning (`envValue`).
pub fn env_value(suffix: &str, env: &HashMap<String, String>) -> Option<String> {
    for name in brand_env_names(suffix, Some(&brand_profile())) {
        if let Some(value) = env.get(&name) {
            return Some(value.clone());
        }
    }
    None
}

fn current_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_profile() {
        let profile = parse_brand_profile(Some(
            r#"{"name":"omo","command":"omo","configDir":".omo","flatLayout":true,"envPrefix":"omo",
                "userAgent":"omo-ai","originator":"omo",
                "update":{"packageName":"omo-ai","distTag":"beta","command":"npm i -g omo-ai@beta","changelogUrl":"https://x/{version}"},
                "changelog":{"path":"/tmp/CHANGELOG.md","version":"1.2.3"}}"#,
        ))
        .expect("profile");
        assert_eq!(profile.name, "omo");
        assert_eq!(profile.config_dir, ".omo");
        assert!(profile.flat_layout);
        assert_eq!(profile.env_prefix, "OMO");
        assert_eq!(profile.user_agent, "omo-ai");
        assert_eq!(profile.originator.as_deref(), Some("omo"));
        assert_eq!(profile.update.as_ref().map(|u| u.dist_tag.as_str()), Some("beta"));
        assert_eq!(profile.changelog.as_ref().and_then(|c| c.version.as_deref()), Some("1.2.3"));
    }

    #[test]
    fn defaults_config_dir_to_dot_name_and_dist_tag_to_latest() {
        let profile = parse_brand_profile(Some(r#"{"name":"tau","update":{"packageName":"tau","command":"npm i -g tau"}}"#))
            .expect("profile");
        assert_eq!(profile.config_dir, ".tau");
        assert_eq!(profile.env_prefix, "TAU");
        assert_eq!(profile.update.as_ref().map(|u| u.dist_tag.as_str()), Some("latest"));
    }

    #[test]
    fn rejects_absent_malformed_and_nameless_input() {
        assert!(parse_brand_profile(None).is_none());
        assert!(parse_brand_profile(Some("   ")).is_none());
        assert!(parse_brand_profile(Some("not json")).is_none());
        assert!(parse_brand_profile(Some("[]")).is_none());
        assert!(parse_brand_profile(Some(r#"{"command":"x"}"#)).is_none());
    }

    #[test]
    fn rejects_unsafe_config_dir() {
        assert!(parse_brand_profile(Some(r#"{"name":"x","configDir":"../evil"}"#)).is_none());
        assert!(parse_brand_profile(Some(r#"{"name":"x","configDir":"."}"#)).is_none());
        assert!(parse_brand_profile(Some(r#"{"name":"x","configDir":"a/b"}"#)).is_none());
    }

    #[test]
    fn rejects_changelog_without_absolute_path() {
        let profile = parse_brand_profile(Some(r#"{"name":"x","changelog":{"path":"relative.md"}}"#)).expect("profile");
        assert!(profile.changelog.is_none());
    }

    #[test]
    fn brand_env_names_put_the_profile_prefix_first_and_dedupe() {
        let profile = parse_brand_profile(Some(r#"{"name":"omo","envPrefix":"senpi"}"#)).expect("profile");
        assert_eq!(
            brand_env_names("CODING_AGENT_DIR", Some(&profile)),
            vec!["SENPI_CODING_AGENT_DIR", "MAHO_CODING_AGENT_DIR", "PI_CODING_AGENT_DIR"]
        );
        assert_eq!(
            brand_env_names("X", None),
            vec!["MAHO_X", "SENPI_X", "PI_X"]
        );
    }

    #[test]
    fn standalone_profile_is_maho() {
        let profile = standalone_brand_profile();
        assert_eq!(profile.name, "maho");
        assert_eq!(profile.config_dir, ".maho");
        assert_eq!(profile.command.as_deref(), Some("mhc"));
    }

    #[test]
    fn env_value_prefers_the_brand_prefix_and_keeps_empty_values() {
        let env = HashMap::from([
            ("MAHO_SETTING".to_owned(), "maho".to_owned()),
            ("SENPI_SETTING".to_owned(), "senpi".to_owned()),
            ("PI_SETTING".to_owned(), String::new()),
        ]);
        assert_eq!(env_value("SETTING", &env).as_deref(), Some("maho"));
        let env = HashMap::from([("PI_SETTING".to_owned(), String::new())]);
        assert_eq!(env_value("SETTING", &env).as_deref(), Some(""));
        assert!(env_value("MISSING", &HashMap::new()).is_none());
    }
}
