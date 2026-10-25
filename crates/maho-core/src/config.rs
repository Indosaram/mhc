//! Port of the user-config path layer of senpi `packages/coding-agent/src/config.ts`.
//!
//! The engine's own identity is maho (`maho` / `.omo` / `mhc`); an injected brand profile
//! overrides it, and the legacy `SENPI_`/`PI_` prefixes stay readable.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::brand::{BrandProfile, brand_profile, env_value};
use crate::paths::normalize_path;

pub const FLAT_LAYOUT_SENTINEL: &str = "settings.json";

pub fn app_name() -> String {
    brand_profile().name
}

pub fn app_command() -> String {
    let profile = brand_profile();
    profile.command.clone().unwrap_or(profile.name)
}

/// Product title; senpi's `APP_TITLE` is the brand name (with `π` for an unbranded install).
pub fn app_title() -> String {
    let profile = brand_profile();
    if profile.name == "maho" {
        "maho code".to_owned()
    } else {
        profile.name
    }
}

pub fn config_dir_name() -> String {
    brand_profile().config_dir
}

pub fn config_flat_layout() -> bool {
    brand_profile().flat_layout
}

pub fn env_prefix() -> String {
    brand_profile().env_prefix
}

pub fn env_agent_dir_var() -> String {
    format!("{}_CODING_AGENT_DIR", env_prefix())
}

pub fn env_session_dir_var() -> String {
    format!("{}_CODING_AGENT_SESSION_DIR", env_prefix())
}

pub fn agent_dir_label() -> String {
    if config_flat_layout() {
        format!("~/{}", config_dir_name())
    } else {
        format!("~/{}/agent", config_dir_name())
    }
}

/// `resolveAgentDir(cwd, homeDir, envDir)`.
pub fn resolve_agent_dir(cwd: &str, home_dir: &str, env_dir: Option<&str>) -> String {
    if let Some(env_dir) = env_dir.filter(|value| !value.is_empty()) {
        return normalize_path(
            env_dir,
            &crate::paths::PathInputOptions {
                home_dir: Some(home_dir.to_owned()),
                ..Default::default()
            },
        );
    }
    let config_dir = config_dir_name();
    // Standalone mhc shares omo's `.omo` home, and omo pins its agent dir to `~/.omo/agent`
    // (launcher `canonicalAgentDir`). Projects keep omo state under `.omo/` too, so a project
    // `.omo/agent` (e.g. project skills) must not be taken for a separate, login-less agent home.
    if crate::brand::injected_brand_profile().is_none() {
        return join(&join(home_dir, &config_dir), "agent");
    }
    if config_flat_layout() {
        let flat = crate::nearest_parent_config::find_nearest_parent_config_dir(
            cwd,
            home_dir,
            &config_dir,
            None,
            Some(FLAT_LAYOUT_SENTINEL),
        );
        return flat.unwrap_or_else(|| join(home_dir, &config_dir));
    }
    match crate::nearest_parent_config::find_nearest_parent_config_dir(
        cwd,
        home_dir,
        &config_dir,
        Some("agent"),
        None,
    ) {
        Some(project) => join(&project, "agent"),
        None => join(&join(home_dir, &config_dir), "agent"),
    }
}

fn join(base: &str, child: &str) -> String {
    Path::new(base).join(child).to_string_lossy().into_owned()
}

const KNOWN_BRAND_CONFIG_DIRS: &[&str] = &[".senpi", ".omo", ".pi"];

/// A test run resolved the real user's agent directory, where its writes would land in live state (#3867).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Refusing to use the real agent directory {agent_dir} from a test run. \
     Point <BRAND>_CODING_AGENT_DIR at a temp dir, or set SENPI_TEST_USE_REAL_AGENT_DIR=1 to opt in."
)]
pub struct RealAgentDirInTestError {
    pub agent_dir: String,
}

fn is_guarded_test_run(env: &HashMap<String, String>) -> bool {
    env.get("SENPI_TEST_AGENT_DIR_GUARD").map(|v| v.as_str()) == Some("1")
}

fn real_home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::getuid()) {
            return Some(user.dir);
        }
    }
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

/// Throws `RealAgentDirInTestError` when a test run (`SENPI_TEST_AGENT_DIR_GUARD=1`) resolves an agent
/// directory inside the real user's brand config directory.
pub fn assert_agent_dir_allowed_in_tests(
    agent_dir: &str,
    active_config_dir_name: &str,
    env: &HashMap<String, String>,
) -> Result<(), RealAgentDirInTestError> {
    if !is_guarded_test_run(env)
        || env.get("SENPI_TEST_USE_REAL_AGENT_DIR").map(|v| v.as_str()) == Some("1")
    {
        return Ok(());
    }
    let Some(home) = real_home_dir() else {
        return Ok(());
    };
    let target = Path::new(agent_dir);
    let target_canonical = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    let mut brand_dirs = KNOWN_BRAND_CONFIG_DIRS.to_vec();
    if !brand_dirs.contains(&active_config_dir_name) {
        brand_dirs.push(active_config_dir_name);
    }
    for config_dir in brand_dirs {
        let root = home.join(config_dir);
        let root_canonical = root.canonicalize().unwrap_or_else(|_| root.clone());
        if target_canonical == root_canonical || target_canonical.starts_with(&root_canonical) {
            return Err(RealAgentDirInTestError {
                agent_dir: agent_dir.to_owned(),
            });
        }
    }
    Ok(())
}

/// `getAgentDir()`: resolves against the process cwd, `$HOME` and the env override.
pub fn get_agent_dir() -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_owned());
    let home = home_dir();
    let env = current_env();
    let env_override = env_value("CODING_AGENT_DIR", &env);
    let agent_dir = resolve_agent_dir(&cwd, &home, env_override.as_deref());
    if let Err(error) = assert_agent_dir_allowed_in_tests(&agent_dir, &config_dir_name(), &env) {
        panic!("{}", error);
    }
    agent_dir
}

/// `getBinDir()`: managed helper binaries live under the agent dir.
pub fn get_bin_dir() -> String {
    Path::new(&get_agent_dir())
        .join("bin")
        .to_string_lossy()
        .into_owned()
}

pub fn get_agent_dir_with(cwd: &str, home_dir: &str, env: &HashMap<String, String>) -> String {
    let env_dir = env_value("CODING_AGENT_DIR", env);
    resolve_agent_dir(cwd, home_dir, env_dir.as_deref())
}

pub fn get_sessions_dir() -> String {
    join(&get_agent_dir(), "sessions")
}

pub fn get_auth_path() -> String {
    join(&get_agent_dir(), "auth.json")
}

pub fn get_settings_path() -> String {
    join(&get_agent_dir(), "settings.json")
}

pub fn get_models_path() -> String {
    join(&get_agent_dir(), "models.json")
}

pub fn get_trust_path() -> String {
    join(&get_agent_dir(), "trust.json")
}

pub fn get_debug_log_path() -> String {
    join(&get_agent_dir(), &format!("{}-debug.log", app_name()))
}

/// `getPackageDir`: the `PACKAGE_DIR` override, else the running executable's directory.
pub fn get_package_dir() -> String {
    if let Some(env_dir) =
        env_value("PACKAGE_DIR", &current_env()).filter(|value| !value.is_empty())
    {
        return normalize_path(&env_dir, &crate::paths::PathInputOptions::default());
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent()
                .map(|parent| parent.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// `getReadmePath`.
pub fn get_readme_path() -> String {
    resolve_in_package("README.md")
}

/// `getDocsPath`.
pub fn get_docs_path() -> String {
    resolve_in_package("docs")
}

/// `getExamplesPath`.
pub fn get_examples_path() -> String {
    resolve_in_package("examples")
}

fn resolve_in_package(child: &str) -> String {
    crate::paths::lexical_resolve(&Path::new(&get_package_dir()).join(child).to_string_lossy())
}

/// `homedir()`.
pub fn home_dir() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".".to_owned())
}

pub fn home_dir_path() -> PathBuf {
    PathBuf::from(home_dir())
}

pub fn current_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

/// senpi `getShareViewerBase`: the configured share viewer base (`<PREFIX>_SHARE_VIEWER_URL`, trimmed),
/// `None` when unset or empty. There is no default viewer: a gist link is all `/share` shows then.
pub fn get_share_viewer_base() -> Option<String> {
    env_value("SHARE_VIEWER_URL", &current_env())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// senpi `isShareDisabledByEnvironment`: `<PREFIX>_DISABLE_SHARE` set to anything but empty, `0` or `false`.
pub fn is_share_disabled_by_environment() -> bool {
    env_value("DISABLE_SHARE", &current_env()).is_some_and(|value| {
        let value = value.trim().to_lowercase();
        !value.is_empty() && value != "0" && value != "false"
    })
}

/// A brand profile's display version, else the engine version.
pub fn display_version(engine_version: &str) -> String {
    brand_profile()
        .display_version
        .unwrap_or_else(|| engine_version.to_owned())
}

pub fn brand() -> BrandProfile {
    brand_profile()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_override_falls_back_to_the_home_agent_dir() {
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let project = root.path().join("project");
        std::fs::create_dir_all(project.join(config_dir_name()).join("agent")).expect("agent");
        // When
        let resolved = resolve_agent_dir(
            &project.to_string_lossy(),
            &home.to_string_lossy(),
            Some(""),
        );
        // Then
        assert_eq!(
            resolved,
            home.join(config_dir_name()).join("agent").to_string_lossy()
        );
    }

    #[test]
    fn resolves_the_default_agent_dir_under_the_config_dir() {
        let expected = Path::new("/home/u")
            .join(config_dir_name())
            .join("agent")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            resolve_agent_dir("/work/project", "/home/u", None),
            expected
        );
    }

    #[test]
    fn an_env_override_wins_and_expands_tilde() {
        let expected = Path::new("/home/u")
            .join("custom")
            .to_string_lossy()
            .into_owned();
        assert_eq!(
            resolve_agent_dir("/work", "/home/u", Some("~/custom")),
            expected
        );
        assert_eq!(
            resolve_agent_dir("/work", "/home/u", Some("/abs/dir")),
            "/abs/dir"
        );
    }

    #[test]
    fn a_project_omo_agent_dir_does_not_replace_the_home_agent_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(project.join(config_dir_name()).join("agent")).expect("mkdir");
        std::fs::create_dir_all(project.join("nested").join("deep")).expect("mkdir");
        std::fs::create_dir_all(&home).expect("mkdir");
        let resolved = resolve_agent_dir(
            &project.join("nested").join("deep").to_string_lossy(),
            &home.to_string_lossy(),
            None,
        );
        assert_eq!(
            resolved,
            home.join(config_dir_name()).join("agent").to_string_lossy()
        );
    }

    #[test]
    fn agent_dir_label_follows_the_layout() {
        assert_eq!(agent_dir_label(), "~/.omo/agent");
    }

    #[test]
    fn assert_agent_dir_allowed_in_tests_blocks_brand_config_dir_when_guarded() {
        let mut env = HashMap::new();
        env.insert("SENPI_TEST_AGENT_DIR_GUARD".to_owned(), "1".to_owned());
        if let Some(home) = real_home_dir() {
            let forbidden = home.join(".senpi").join("agent");
            let result =
                assert_agent_dir_allowed_in_tests(&forbidden.to_string_lossy(), ".senpi", &env);
            assert!(result.is_err());

            // Opt in allows it
            env.insert("SENPI_TEST_USE_REAL_AGENT_DIR".to_owned(), "1".to_owned());
            let result =
                assert_agent_dir_allowed_in_tests(&forbidden.to_string_lossy(), ".senpi", &env);
            assert!(result.is_ok());
        }
    }
}
