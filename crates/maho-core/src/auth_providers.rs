//! Port of senpi packages/coding-agent/src/core/auth-providers.ts: non-UI login and logout provider selection.
use crate::model_registry::ModelRegistry;
use crate::auth_storage::CredentialKind;
use std::collections::HashSet;

#[derive(Debug,Clone,PartialEq,Eq)]
pub struct AuthProviderInfo {pub id:String,pub name:String,pub auth_type:CredentialKind}

/// senpi `isApiKeyLoginProvider`: a built-in display name always qualifies, another built-in model
/// provider never does, and any other provider qualifies unless it is an OAuth provider.
pub fn is_api_key_login_provider(id:&str,oauth:&HashSet<String>,builtins:&HashSet<String>) -> bool {
    if crate::provider_display_names::built_in_provider_display_names().contains_key(id) {return true;}
    if builtins.contains(id) {return false;}
    !oauth.contains(id)
}

pub fn build_login_provider_infos(registry:&ModelRegistry,auth_type:Option<CredentialKind>) -> Vec<AuthProviderInfo> {
    let oauth=oauth_infos(&registry.auth_storage);let ids:HashSet<_>=oauth.iter().map(|p|p.id.clone()).collect();
    let builtin:HashSet<_>=maho_ai::providers::all::get_builtin_providers().into_iter().map(str::to_owned).collect();
    let mut options=oauth;let mut seen=HashSet::new();
    for model in registry.get_all() {
        if seen.insert(model.provider.clone())&&is_api_key_login_provider(&model.provider,&ids,&builtin){options.push(AuthProviderInfo{name:registry.get_provider_display_name(&model.provider),id:model.provider,auth_type:CredentialKind::ApiKey});}
    }
    options.retain(|p|auth_type.is_none_or(|kind|kind==p.auth_type));options.sort_by(|a,b|a.name.cmp(&b.name));options
}

fn oauth_infos(storage:&crate::auth_storage::AuthStorage)->Vec<AuthProviderInfo> {
    storage.get_oauth_providers().into_iter().map(|p|AuthProviderInfo{id:p.id,name:p.name,auth_type:CredentialKind::Oauth}).collect()
}

/// The builtin OAuth providers (no extension registrations), for callers without a registry.
pub fn oauth_provider_infos()->Vec<AuthProviderInfo> {
    oauth_infos(&crate::auth_storage::AuthStorage::in_memory(Default::default()))
}

pub fn build_logout_provider_infos(registry:&ModelRegistry) -> Vec<AuthProviderInfo> {
    let mut options:Vec<_> = registry.auth_storage.list().into_iter().map(|(id,auth_type)|AuthProviderInfo{name:registry.get_provider_display_name(&id),id,auth_type}).collect();
    options.sort_by(|a,b|a.name.cmp(&b.name));options
}
