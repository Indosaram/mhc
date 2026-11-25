//! Port of senpi `packages/coding-agent/src/core/runtime/credential-resolver.ts`.

use maho_ai::model::Model;

use crate::model_registry::{ModelRegistry, ResolvedRequestAuth};

pub trait CredentialResolver: Send + Sync {
    fn get_all_models(&self) -> Vec<Model>;
    fn get_available_models(&self) -> Vec<Model>;
    fn get_api_key_and_headers<'a>(
        &'a self,
        model: &'a Model,
    ) -> maho_ai::types::BoxFuture<'a, ResolvedRequestAuth>;
    fn provider_auth_is_header_only(&self, provider: &str) -> bool;
}

impl CredentialResolver for ModelRegistry {
    fn get_all_models(&self) -> Vec<Model> {
        self.get_all()
    }

    fn get_available_models(&self) -> Vec<Model> {
        self.get_available()
    }

    fn get_api_key_and_headers<'a>(
        &'a self,
        model: &'a Model,
    ) -> maho_ai::types::BoxFuture<'a, ResolvedRequestAuth> {
        Box::pin(async move { self.get_api_key_and_headers(model).await })
    }

    fn provider_auth_is_header_only(&self, provider: &str) -> bool {
        self.provider_auth_is_header_only(provider)
    }
}
