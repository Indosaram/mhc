//! Non-UI login and logout provider selection.
use crate::model_registry::ModelRegistry;
use crate::auth_storage::CredentialKind;
use std::collections::HashSet;

#[derive(Debug,Clone,PartialEq,Eq)]
pub struct AuthProviderInfo {pub id:String,pub name:String,pub auth_type:CredentialKind}

pub fn is_api_key_login_provider(id:&str,oauth:&HashSet<String>,builtins:&HashSet<String>,display_names:&HashSet<String>) -> bool {
    if display_names.contains(id) {return true;}
    if builtins.contains(id) {return false;}
    !oauth.contains(id)
}

pub fn build_login_provider_infos(registry:&ModelRegistry,auth_type:Option<CredentialKind>) -> Vec<AuthProviderInfo> {
    let oauth=oauth_provider_infos();let ids:HashSet<_>=oauth.iter().map(|p|p.id.clone()).collect();
    let builtin:HashSet<_>=maho_ai::providers::all::get_builtin_providers().into_iter().map(str::to_owned).collect();
    let names:HashSet<_>=["anthropic","anthropic-subscription","amazon-bedrock","ant-ling","azure-openai-responses","cerebras","cloudflare-ai-gateway","cloudflare-workers-ai","cursor","cursor-cli-oauth","deepseek","fireworks","google","google-vertex","groq","huggingface","kimi-coding","minimax","minimax-cn","moonshotai","moonshotai-cn","nvidia","opencode","opencode-go","openai","chatgpt-subscription","opengateway","ollama","openrouter","together","venice","vercel-ai-gateway","xai","zai","zai-coding-cn","xiaomi","xiaomi-token-plan-cn","xiaomi-token-plan-ams","xiaomi-token-plan-sgp","alibaba-token-plan"].into_iter().map(str::to_owned).collect();
    let mut options=oauth;let mut seen=HashSet::new();
    for model in registry.get_all() {
        if seen.insert(model.provider.clone())&&is_api_key_login_provider(&model.provider,&ids,&builtin,&names){options.push(AuthProviderInfo{name:registry.get_provider_display_name(&model.provider),id:model.provider,auth_type:CredentialKind::ApiKey});}
    }
    options.retain(|p|auth_type.is_none_or(|kind|kind==p.auth_type));options.sort_by(|a,b|a.name.cmp(&b.name));options
}

pub fn oauth_provider_infos()->Vec<AuthProviderInfo> {
    [("anthropic","Anthropic"),("chatgpt-subscription","ChatGPT Subscription"),("github-copilot","GitHub Copilot"),("openrouter","OpenRouter"),("kimi-coding","Kimi For Coding"),("xai","xAI"),("cursor","Cursor"),("devin","Devin"),("radius","Radius")].into_iter().map(|(id,name)|AuthProviderInfo{id:id.into(),name:name.into(),auth_type:CredentialKind::Oauth}).collect()
}

pub fn build_logout_provider_infos(registry:&ModelRegistry) -> Vec<AuthProviderInfo> {
    let mut options:Vec<_> = registry.auth_storage.list().into_iter().map(|(id,auth_type)|AuthProviderInfo{name:registry.get_provider_display_name(&id),id,auth_type}).collect();
    options.sort_by(|a,b|a.name.cmp(&b.name));options
}
