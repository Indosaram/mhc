use maho_ext_api::{CredentialAccountSource, ExtensionFuture, Model, ModelRegistry};

struct LegacyRegistry;
impl ModelRegistry for LegacyRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}

#[tokio::test]
async fn legacy_registry_refuses_all_unbound_account_operations() {
    let registry: &dyn ModelRegistry = &LegacyRegistry;
    assert!(registry.get_credential_accounts("synthetic").await.is_err());
    assert!(registry.pin_credential_account("synthetic", Some("first")).await.is_err());
    assert!(registry.pin_credential_account("synthetic", None).await.is_err());
    assert!(registry.remove_credential_account("synthetic", "first").await.is_err());
    assert!(registry.rename_credential_account("synthetic", "first", Some("Work")).await.is_err());
    assert!(registry.rename_credential_account("synthetic", "first", None).await.is_err());
}

#[test]
fn account_sources_keep_pinned_source_discriminants() {
    assert_eq!(CredentialAccountSource::Login.as_str(), "login");
    assert_eq!(CredentialAccountSource::Import.as_str(), "import");
    assert_eq!(CredentialAccountSource::Env.as_str(), "env");
}
