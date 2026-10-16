use maho_ext_api::{ExtensionSessionSettings,ModelRegistry,RetryFallbackSettings,FlagValue};
use maho_core::retry_fallback::{chains::canonicalize_fallback_chains,expansion::FallbackAuthTiers};
pub fn is_model_fallback_disabled(flag:Option<&FlagValue>,environment:Option<&str>)->bool{flag==Some(&FlagValue::Boolean(true))||environment==Some("1")}
pub fn load_fallback_settings(settings:&dyn ExtensionSessionSettings,models:&dyn ModelRegistry,is_using_oauth:&dyn Fn(&maho_ext_api::Model)->bool)->RetryFallbackSettings{
    let mut settings=settings.get_retry_fallback_settings();let available=models.get_available();let available=if available.is_empty(){models.get_all()}else{available};
    let auth=|model:&maho_ext_api::Model|models.has_configured_auth(model);
    let tiers=FallbackAuthTiers{is_using_oauth,has_configured_auth:Some(&auth),is_fallback_eligible:None};
    let chains=settings.chains.iter().map(|(key,value)|(key.clone(),value.clone())).collect();
    settings.chains=canonicalize_fallback_chains(&chains,&available,&tiers).into_iter().collect();settings
}

#[cfg(test)]
mod tests {
    use maho_ai::{model::Model, types::ModelCost};
    use maho_core::retry_fallback::expansion::{rank_family_models, FallbackAuthTiers};

    fn model(provider: &str) -> Model {
        Model { id: "example-v1".into(), name: "Example".into(), api: "openai-completions".into(), provider: provider.into(), base_url: String::new(), reasoning: false, thinking_level_map: None, input: vec![], cost: ModelCost::default(), context_window: 4096, max_tokens: 1024, sampling_params: None, headers: None, cache_retention: None, upstream_model_id: None, service_tier: None, recover_text_tool_calls: None, compat: None }
    }

    #[test]
    fn oauth_predicate_is_used_for_available_model_ranking() {
        let models = vec![model("ordinary"), model("oauth")];
        let oauth = |model: &Model| model.provider == "oauth";
        let tiers = FallbackAuthTiers { is_using_oauth: &oauth, has_configured_auth: None, is_fallback_eligible: None };
        let ranked = rank_family_models(&models, "example", &tiers, None);
        assert_eq!(ranked.first().map(|model| model.provider.as_str()), Some("oauth"));
    }
}
