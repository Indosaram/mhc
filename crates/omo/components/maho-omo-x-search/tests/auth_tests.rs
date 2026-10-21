//! Port of omo-senpi `components/x-search/auth.test.ts` at latest `455dee62`.

use maho_ext_api::{ExtensionFuture, ModelRegistry};
use maho_omo_x_search::auth::{XAI_PROVIDER, has_xai_credential, resolve_xai_bearer};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;

struct MockModelRegistry {
    stored_cred_type: Option<maho_ai::auth::types::CredentialType>,
    provider_auth: Option<Result<Option<maho_ai::models::AuthResolution>, String>>,
}

impl ModelRegistry for MockModelRegistry {
    fn get_all(&self) -> Vec<maho_ai::model::Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<maho_ai::model::Model> {
        Vec::new()
    }
    fn find(&self, _provider: &str, _id: &str) -> Option<maho_ai::model::Model> {
        None
    }
    fn has_configured_auth(&self, _model: &maho_ai::model::Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(
        &'a self,
        _provider: &'a str,
    ) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
    fn get_stored_credential_type(
        &self,
        provider: &str,
    ) -> Result<Option<maho_ai::auth::types::CredentialType>, maho_ext_api::ExtensionFailure> {
        if provider == XAI_PROVIDER {
            Ok(self.stored_cred_type)
        } else {
            Ok(None)
        }
    }
    fn get_provider_auth<'a>(
        &'a self,
        provider: &'a str,
    ) -> ExtensionFuture<'a, Option<maho_ai::models::AuthResolution>> {
        let auth = self.provider_auth.clone();
        Box::pin(async move {
            if provider != XAI_PROVIDER {
                return Ok(None);
            }
            match auth {
                Some(Ok(res)) => Ok(res),
                Some(Err(err)) => Err(maho_ext_api::ExtensionFailure::new(err)),
                None => Ok(None),
            }
        })
    }
}

fn make_registry(
    stored: Option<maho_ai::auth::types::CredentialType>,
    api_key: Option<&str>,
) -> MockModelRegistry {
    MockModelRegistry {
        stored_cred_type: stored,
        provider_auth: Some(Ok(api_key.map(|key| maho_ai::models::AuthResolution {
            ambient: false,
            auth: maho_ai::models::ProviderAuthResult {
                api_key: Some(key.to_string()),
                ..Default::default()
            },
            env: None,
        }))),
    }
}

fn write_auth_json(dir: &std::path::Path, value: &serde_json::Value) {
    let mut file = File::create(dir.join("auth.json")).expect("create auth.json");
    let content = serde_json::to_string(value).expect("serialize json");
    file.write_all(content.as_bytes()).expect("write auth.json");
}

#[tokio::test]
async fn test_stored_credential_uses_registry_bearer() {
    let registry = make_registry(
        Some(maho_ai::auth::types::CredentialType::OAuth),
        Some("stored-token"),
    );
    let mut env = BTreeMap::new();
    env.insert("XAI_API_KEY".to_string(), "env".to_string());

    let res = resolve_xai_bearer(&registry, &env).await;
    assert_eq!(res, Some(("stored-token".to_string(), "store")));
}

#[tokio::test]
async fn test_stored_credential_failure_fails_closed() {
    let registry = MockModelRegistry {
        stored_cred_type: Some(maho_ai::auth::types::CredentialType::OAuth),
        provider_auth: Some(Err("refresh failed".to_string())),
    };
    let mut env = BTreeMap::new();
    env.insert("XAI_API_KEY".to_string(), "env".to_string());

    let res = resolve_xai_bearer(&registry, &env).await;
    assert_eq!(res, None);
}

#[tokio::test]
async fn test_no_stored_credential_uses_trimmed_env() {
    let registry = make_registry(None, None);
    let mut env = BTreeMap::new();
    env.insert("XAI_API_KEY".to_string(), "  env-token ".to_string());

    let res = resolve_xai_bearer(&registry, &env).await;
    assert_eq!(res, Some(("env-token".to_string(), "env")));
}

#[tokio::test]
async fn test_neither_source_is_undefined() {
    let registry = make_registry(None, None);
    let env = BTreeMap::new();

    let res = resolve_xai_bearer(&registry, &env).await;
    assert_eq!(res, None);
}

#[test]
fn test_credential_gate_recognizes_oauth_and_api_key() {
    let td1 = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        td1.path(),
        &serde_json::json!({ "xai": { "type": "oauth" } }),
    );
    assert!(has_xai_credential(td1.path(), &BTreeMap::new()));

    let td2 = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        td2.path(),
        &serde_json::json!({ "xai": { "type": "api_key" } }),
    );
    assert!(has_xai_credential(td2.path(), &BTreeMap::new()));
}

#[test]
fn test_gate_handles_missing_invalid_and_unknown_entries() {
    let missing_path = std::path::Path::new("/missing_nonexistent_dir_12345");
    assert!(!has_xai_credential(missing_path, &BTreeMap::new()));

    let td_invalid = tempfile::tempdir().expect("tempdir");
    let mut f = File::create(td_invalid.path().join("auth.json")).expect("create");
    f.write_all(b"{").expect("write");
    let mut env = BTreeMap::new();
    env.insert("XAI_API_KEY".to_string(), "env".to_string());
    assert!(!has_xai_credential(td_invalid.path(), &env));

    let td_unknown = tempfile::tempdir().expect("tempdir");
    write_auth_json(
        td_unknown.path(),
        &serde_json::json!({ "xai": { "type": "other" } }),
    );
    assert!(!has_xai_credential(td_unknown.path(), &env));
}

#[test]
fn test_env_gates_missing_file() {
    let missing_path = std::path::Path::new("/missing_nonexistent_dir_12345");
    let mut env = BTreeMap::new();
    env.insert("XAI_API_KEY".to_string(), " env ".to_string());
    assert!(has_xai_credential(missing_path, &env));
}
