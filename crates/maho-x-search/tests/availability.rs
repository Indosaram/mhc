//! §8.1 load gate and §7 offline skip.

use crate::support;
use maho_ext_api::{EventKind, Extension};
use maho_x_search::{EnvLookup, XSearchExtension, availability};
use std::path::Path;
use std::sync::Arc;

pub fn env_of(pairs: &[(&str, &str)]) -> EnvLookup {
    let pairs: Vec<(String, String)> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    Arc::new(move |key: &str| {
        pairs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    })
}

pub fn extension(agent_dir: &Path, env: EnvLookup) -> XSearchExtension {
    let mut extension = XSearchExtension::new(agent_dir);
    extension.env = env;
    extension.disabled_skills = Arc::new(|_: &Path| Vec::new());
    extension.warn = Arc::new(|_: &str| {});
    extension
}

/// Registers and reports (tool registered, skill handler registered).
fn load(auth: Option<&str>, env: &[(&str, &str)]) -> (bool, bool) {
    let dir = tempfile::tempdir().expect("dir");
    if let Some(auth) = auth {
        std::fs::write(dir.path().join("auth.json"), auth).expect("auth");
    }
    let mut api = support::api(dir.path());
    extension(dir.path(), env_of(env)).register(&mut api);
    let tool = api
        .registered
        .tools
        .iter()
        .any(|tool| tool.definition.name == "x_search");
    let skill = api
        .registered
        .handlers
        .get(&EventKind::ResourcesDiscover)
        .is_some_and(|handlers| !handlers.is_empty());
    (tool, skill)
}

const KEY: (&str, &str) = ("XAI_API_KEY", "xai-test-key");

#[test]
fn auth_file_with_api_key_or_oauth_registers() {
    assert_eq!(
        load(Some(r#"{"xai": {"type": "api_key", "key": "k"}}"#), &[]),
        (true, true)
    );
    assert_eq!(
        load(Some(r#"{"xai": {"type": "oauth", "access": "a"}}"#), &[]),
        (true, true)
    );
}

#[test]
fn auth_file_without_usable_xai_entry_fails_closed_even_with_env_key() {
    for auth in [
        r#"{"xai": {"type": "token"}}"#,
        r#"{"xai": "api_key"}"#,
        r#"{"openai": {"type": "api_key"}}"#,
        r#"["xai"]"#,
        "{not json",
    ] {
        assert_eq!(load(Some(auth), &[KEY]), (false, false), "auth.json {auth}");
    }
}

#[test]
fn without_auth_file_a_non_blank_env_key_registers() {
    assert_eq!(load(None, &[KEY]), (true, true));
    assert_eq!(load(None, &[("XAI_API_KEY", "   ")]), (false, false));
}

#[test]
fn neither_credential_registers_nothing() {
    assert_eq!(load(None, &[]), (false, false));
}

#[test]
fn unreadable_auth_file_is_not_connected() {
    let dir = tempfile::tempdir().expect("dir");
    // A directory named auth.json exists but cannot be read as a file.
    std::fs::create_dir(dir.path().join("auth.json")).expect("dir");
    let env = env_of(&[KEY]);
    assert!(!availability::is_connected(dir.path(), env.as_ref()));
}

#[test]
fn offline_mode_skips_registration() {
    assert_eq!(load(None, &[KEY, ("MAHO_OFFLINE", "1")]), (false, false));
    assert_eq!(load(None, &[KEY, ("PI_OFFLINE", "true")]), (false, false));
    assert_eq!(load(None, &[KEY, ("PI_OFFLINE", "0")]), (true, true));
}
