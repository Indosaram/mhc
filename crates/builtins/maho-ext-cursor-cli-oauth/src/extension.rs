use std::sync::Arc;
use maho_ext_api::{Extension,ExtensionApi,ProviderConfig};
pub struct CursorCliExtension {
    pub oauth:Arc<crate::oauth_login::CursorCliOAuth>,
    pub stream:maho_ext_api::types::ProviderStream,
    pub settings:Arc<dyn Fn()->crate::settings::CursorCliOauthProviderSettings+Send+Sync>,
}
impl Extension for CursorCliExtension {
    fn register(&self,api:&mut ExtensionApi) {
        crate::account_command::register(api,self.oauth.clone());
        let settings=self.settings.clone();
        let config=ProviderConfig {name:Some(crate::oauth_login::PROVIDER_NAME.into()),base_url:Some(crate::oauth_login::PROVIDER_ID.into()),api:Some(crate::oauth_login::PROVIDER_ID.into()),
            stream_simple:Some(self.stream.clone()),models:Some(crate::models::static_models()),oauth:Some(self.oauth.clone()),fallback_eligible:Some(Arc::new(move || {let current=settings();!current.explicitly_disabled&&!crate::guardrails::force_refusal_pending(&current)})),..Default::default()};
        api.register_provider(crate::oauth_login::PROVIDER_ID,config).expect("Cursor CLI provider registration");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::types::*;
    struct Capture(std::sync::Mutex<Vec<ProviderRegistration>>);
    impl ExtensionProviderActions for Capture {
        fn register_provider(&self,registration:ProviderRegistration,_:&str)->Result<(),ExtensionFailure> {self.0.lock().expect("capture").push(registration);Ok(())}
        fn unregister_provider(&self,_:&str,_:&str)->Result<(),ExtensionFailure> {Ok(())}
    }
    #[test]
    fn registers_offline_lane_and_live_fallback_policy() {
        let disabled=Arc::new(std::sync::atomic::AtomicBool::new(false));let settings={let disabled=disabled.clone();Arc::new(move ||crate::settings::CursorCliOauthProviderSettings {explicitly_disabled:disabled.load(std::sync::atomic::Ordering::SeqCst),execution_mode:crate::settings::ExecutionMode::Plan,..Default::default()})};
        let oauth=Arc::new(crate::oauth_login::CursorCliOAuth {store:Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new()),flow:Arc::new(maho_ai::auth::oauth::cursor::CursorOAuth::new()),settings:settings.clone(),resolve:Arc::new(|_|Err(anyhow::anyhow!("not installed"))),persist_acknowledgement:Arc::new(|_|Ok(())),persist_enabled:Arc::new(|_|Ok(())),now:Arc::new(||1)});
        let extension=CursorCliExtension {oauth,settings,stream:Arc::new(|_,_,_|panic!("registration never streams"))};
        let runtime=ExtensionRuntime::default();let mut api=ExtensionApi::new(LoadedExtension::new("cursor","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime.clone());extension.register(&mut api);
        let capture=Arc::new(Capture(Default::default()));runtime.bind_providers(capture.clone()).expect("bind");let registrations=capture.0.lock().expect("registrations");assert_eq!(registrations.len(),1);
        let ProviderRegistration::Config {name,config}=&registrations[0] else {panic!("config");};assert_eq!(name,crate::oauth_login::PROVIDER_ID);assert_eq!(config.models.as_ref().expect("offline models").len(),15);assert!(config.oauth.is_some());assert!(config.stream_simple.is_some());let eligible=config.fallback_eligible.as_ref().expect("fallback");assert!(eligible());disabled.store(true,std::sync::atomic::Ordering::SeqCst);assert!(!eligible());
    }
}
