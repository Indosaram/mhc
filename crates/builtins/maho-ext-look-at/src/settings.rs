use maho_ext_api::LookAtSettings;
use crate::model_selector::DEFAULT_LOOK_AT_CHAIN;
#[derive(Clone,Debug,Default,PartialEq,Eq)]
pub struct LookAtOverride { pub models:Option<Vec<String>>,pub enabled:Option<bool> }
#[derive(Clone,Debug,Default)]
pub struct LookAtStore { override_settings:LookAtOverride }
impl LookAtStore {
    pub fn get_override(&self)-> &LookAtOverride { &self.override_settings }
    pub fn set_models(&mut self,models:Option<Vec<String>>) { self.override_settings.models=models; }
    pub fn set_enabled(&mut self,enabled:Option<bool>) { self.override_settings.enabled=enabled; }
}
pub fn create_look_at_store()->LookAtStore { LookAtStore::default() }
pub fn load_look_at_chain(settings:&LookAtSettings,store:&LookAtStore)->Vec<String> {
    store.get_override().models.as_ref().or(settings.models.as_ref()).cloned().unwrap_or_else(||DEFAULT_LOOK_AT_CHAIN.into_iter().map(String::from).collect())
}
pub fn load_look_at_enabled(settings:&LookAtSettings,store:&LookAtStore)->bool { store.get_override().enabled.unwrap_or(settings.enabled) }
#[cfg(test)]
mod tests {
    use super::*;
    fn settings(models:Option<Vec<String>>,enabled:bool)->LookAtSettings { LookAtSettings{models,enabled} }
    #[test] fn configured_models_replace_default_chain() { assert_eq!(load_look_at_chain(&settings(Some(vec!["google/gemini-3.5-flash".into()]),true),&create_look_at_store()),vec!["google/gemini-3.5-flash"]); }
    #[test] fn override_wins_over_configured_models() { let mut store=create_look_at_store(); store.set_models(Some(vec!["openai/gpt-5.6-terra".into()])); assert_eq!(load_look_at_chain(&settings(Some(vec!["google/gemini-3.5-flash".into()]),true),&store),vec!["openai/gpt-5.6-terra"]); }
    #[test] fn absent_override_uses_defaults() { let s=settings(None,true); let store=create_look_at_store(); assert!(load_look_at_enabled(&s,&store)); assert_eq!(load_look_at_chain(&s,&store),DEFAULT_LOOK_AT_CHAIN); }
    #[test] fn stores_are_isolated() { let mut first=create_look_at_store(); let second=create_look_at_store(); first.set_enabled(Some(false)); first.set_models(Some(vec!["moonshotai/kimi-k3".into()])); let s=settings(None,true); assert!(!load_look_at_enabled(&s,&first)); assert!(load_look_at_enabled(&s,&second)); assert_eq!(load_look_at_chain(&s,&second),DEFAULT_LOOK_AT_CHAIN); }
    #[test] fn empty_model_override_and_false_are_preserved() { let mut store=create_look_at_store(); store.set_models(Some(vec![])); store.set_enabled(Some(false)); let s=settings(None,true); assert!(load_look_at_chain(&s,&store).is_empty()); assert!(!load_look_at_enabled(&s,&store)); }
    #[test] fn clearing_override_restores_configuration() { let mut store=create_look_at_store(); store.set_enabled(Some(true)); store.set_models(Some(vec![])); store.set_enabled(None); store.set_models(None); let s=settings(Some(vec!["configured".into()]),false); assert!(!load_look_at_enabled(&s,&store)); assert_eq!(load_look_at_chain(&s,&store),vec!["configured"]); }
}
