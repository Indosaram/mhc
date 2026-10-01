//! Port of the user-config path layer of senpi `packages/coding-agent/src/config.ts`.
//!
//! The engine's own identity is maho (`maho` / `.maho` / `mhc`); an injected brand profile
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
    if profile.name == "maho" { "maho code".to_owned() } else { profile.name }
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
    if let Some(env_dir) = env_dir {
        return normalize_path(env_dir, &crate::paths::PathInputOptions { home_dir: Some(home_dir.to_owned()), ..Default::default() });
    }
    let config_dir = config_dir_name();
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
    match crate::nearest_parent_config::find_nearest_parent_config_dir(cwd, home_dir, &config_dir, Some("agent"), None) {
        Some(project) => join(&project, "agent"),
        None => join(&join(home_dir, &config_dir), "agent"),
    }
}

fn join(base: &str, child: &str) -> String {
    Path::new(base).join(child).to_string_lossy().into_owned()
}

/// `getAgentDir()`: resolves against the process cwd, `$HOME` and the env override.
pub fn get_agent_dir() -> String {
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| ".".to_owned());
    let home = home_dir();
    let env = env_value("CODING_AGENT_DIR", &current_env());
    resolve_agent_dir(&cwd, &home, env.as_deref())
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
    if let Some(env_dir) = env_value("PACKAGE_DIR", &current_env()).filter(|value| !value.is_empty()) {
        return normalize_path(&env_dir, &crate::paths::PathInputOptions::default());
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.to_string_lossy().into_owned()))
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
    std::env::var("HOME").ok().filter(|value| !value.is_empty()).unwrap_or_else(|| ".".to_owned())
}

pub fn home_dir_path() -> PathBuf {
    PathBuf::from(home_dir())
}

pub fn current_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

/// A brand profile's display version, else the engine version.
pub fn display_version(engine_version: &str) -> String {
    brand_profile().display_version.unwrap_or_else(|| engine_version.to_owned())
}

pub fn brand() -> BrandProfile {
    brand_profile()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_default_agent_dir_under_the_config_dir() {
        assert_eq!(resolve_agent_dir("/work/project", "/home/u", None), "/home/u/.maho/agent");
    }

    #[test]
    fn an_env_override_wins_and_expands_tilde() {
        assert_eq!(resolve_agent_dir("/work", "/home/u", Some("~/custom")), "/home/u/custom");
        assert_eq!(resolve_agent_dir("/work", "/home/u", Some("/abs/dir")), "/abs/dir");
    }

    #[test]
    fn finds_a_parent_project_config_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(project.join(".maho/agent")).expect("mkdir");
        std::fs::create_dir_all(project.join("nested/deep")).expect("mkdir");
        std::fs::create_dir_all(&home).expect("mkdir");
        let resolved = resolve_agent_dir(
            &project.join("nested/deep").to_string_lossy(),
            &home.to_string_lossy(),
            None,
        );
        assert_eq!(resolved, project.join(".maho/agent").to_string_lossy());
    }

    #[test]
    fn agent_dir_label_follows_the_layout() {
        assert_eq!(agent_dir_label(), "~/.maho/agent");
    }
}
