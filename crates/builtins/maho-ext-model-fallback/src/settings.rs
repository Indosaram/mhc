use maho_ext_api::{ExtensionSessionSettings,ModelRegistry,RetryFallbackSettings,FlagValue};
use maho_core::retry_fallback::{chains::canonicalize_fallback_chains,expansion::FallbackAuthTiers};
pub fn is_model_fallback_disabled(flag:Option<&FlagValue>,environment:Option<&str>)->bool{flag==Some(&FlagValue::Boolean(true))||environment==Some("1")}
pub fn load_fallback_settings(settings:&dyn ExtensionSessionSettings,models:&dyn ModelRegistry)->RetryFallbackSettings{
    let mut settings=settings.get_retry_fallback_settings();let available=models.get_available();let available=if available.is_empty(){models.get_all()}else{available};
    let auth=|model:&maho_ext_api::Model|models.has_configured_auth(model);
    let tiers=FallbackAuthTiers{is_using_oauth:&|_|false,has_configured_auth:Some(&auth),is_fallback_eligible:None};
    let chains=settings.chains.iter().map(|(key,value)|(key.clone(),value.clone())).collect();
    settings.chains=canonicalize_fallback_chains(&chains,&available,&tiers).into_iter().collect();settings
}
