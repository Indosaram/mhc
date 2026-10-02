use std::sync::Arc;
use maho_ext_api::{Extension,ExtensionApi,ProviderConfig};
pub struct CursorCliExtension {
    pub oauth:Arc<crate::oauth_login::CursorCliOAuth>,
    pub stream:maho_ext_api::types::ProviderStream,
    pub settings:Arc<dyn Fn()->crate::settings::CursorCliOauthProviderSettings+Send+Sync>,
    pub router:Option<Arc<tokio::sync::Mutex<crate::session_router::SessionRouter>>>,
    pub shutdown:Option<maho_ai::utils::abort::AbortController>,
    pub catalog:Option<(std::path::PathBuf,std::path::PathBuf,std::collections::BTreeMap<String,String>)>,
    pub catalog_refresh:Option<Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>>,
}
impl CursorCliExtension {
    pub fn native(oauth:Arc<crate::oauth_login::CursorCliOAuth>,executable:std::path::PathBuf,cwd:std::path::PathBuf,agent_dir:std::path::PathBuf,environment:std::collections::BTreeMap<String,String>)->Self {
        let settings=oauth.settings.clone();
        let router=Arc::new(tokio::sync::Mutex::new(crate::session_router::SessionRouter::default()));
        let shutdown=maho_ai::utils::abort::AbortController::new();
        let catalog=Some((executable.clone(),agent_dir.clone(),environment.clone()));
        let stream={let oauth=oauth.clone();let router=router.clone();let shutdown=shutdown.clone();Arc::new(move |model:&maho_ai::model::Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::SimpleStreamOptions>| {
            let mut options=options.unwrap_or_default();
            let controller=maho_ai::utils::abort::AbortController::new();
            let signals=[options.stream.request.signal.clone(),Some(shutdown.signal())];
            let mut subscriptions=Vec::new();
            for signal in signals.into_iter().flatten() {
                let abort=controller.clone();let id=signal.add_abort_listener(move |reason|abort.abort(Some(reason.clone())));
                if signal.aborted() {controller.abort(signal.reason());}
                subscriptions.push((signal,id));
            }
            options.stream.request.signal=Some(controller.signal());
            let stream=crate::stream::stream_cursor_cli(model.clone(),context.clone(),Some(options),crate::stream::StreamDeps {cwd:cwd.clone(),agent_dir:agent_dir.clone(),executable:executable.clone(),store:oauth.store.clone(),oauth:oauth.clone(),settings:(oauth.settings)(),router:router.clone(),environment:environment.clone(),now:oauth.now.clone()});
            let completion=stream.clone();
            tokio::spawn(async move {
                if let Err(error)=completion.result().await {eprintln!("Cursor CLI completion failed: {error}");}
                for (signal,id) in subscriptions {signal.remove_abort_listener(id);}
            });
            stream
        }) as maho_ext_api::types::ProviderStream};
        Self {oauth,settings,stream,catalog,router:Some(router),shutdown:Some(shutdown),catalog_refresh:Some(Arc::new(Default::default()))}
    }
}
impl Extension for CursorCliExtension {
    fn register(&self,api:&mut ExtensionApi) {
        if let Some(router)=&self.router {crate::account_command::register_with_router(api,self.oauth.clone(),router.clone());}else {crate::account_command::register(api,self.oauth.clone());}
        if let Some(shutdown)=&self.shutdown {
            for kind in [maho_ext_api::EventKind::SessionShutdown,maho_ext_api::EventKind::SessionExtensionsRemoved] {
                let shutdown=shutdown.clone();
                let catalog_refresh=self.catalog_refresh.clone();
                api.on(kind,Arc::new(move |_,_| {
                    shutdown.abort(None);
                    let task=catalog_refresh.as_ref().and_then(|task|task.lock().expect("catalog task").take());
                    Box::pin(async move {if let Some(task)=task {task.await.map_err(|error|maho_ext_api::ExtensionFailure::new(error.to_string()))?;}Ok(maho_ext_api::EventResult::None)})
                }));
            }
        }
        let settings=self.settings.clone();
        let config=ProviderConfig {name:Some(crate::oauth_login::PROVIDER_NAME.into()),base_url:Some(crate::oauth_login::PROVIDER_ID.into()),api:Some(crate::oauth_login::PROVIDER_ID.into()),
            stream_simple:Some(self.stream.clone()),models:Some(crate::models::static_models()),oauth:Some(self.oauth.clone()),fallback_eligible:Some(Arc::new(move || {let current=settings();!current.explicitly_disabled&&!crate::guardrails::force_refusal_pending(&current)})),..Default::default()};
        api.register_provider(crate::oauth_login::PROVIDER_ID,config.clone()).expect("Cursor CLI provider registration");
        if let Some((executable,agent_dir,environment))=&self.catalog {
            let executable=executable.clone();let agent_dir=agent_dir.clone();let environment=environment.clone();
            let oauth=self.oauth.clone();let runtime=api.runtime.clone();let path=api.registered.identity.path.clone();
            let catalog_refresh=self.catalog_refresh.clone();
            let shutdown=self.shutdown.clone();
            api.on(maho_ext_api::EventKind::SessionStart,Arc::new(move |_,_| {
                let executable=executable.clone();let agent_dir=agent_dir.clone();let environment=environment.clone();
                let oauth=oauth.clone();let runtime=runtime.clone();let path=path.clone();let mut config=config.clone();
                let catalog_refresh=catalog_refresh.clone();
                let shutdown=shutdown.clone();
                Box::pin(async move {
                    runtime.assert_active()?;
                    if shutdown.as_ref().is_some_and(|shutdown|shutdown.signal().aborted()) {return Ok(maho_ext_api::EventResult::None);}
                    let refresh=async move {
                        let settings=(oauth.settings)();
                        let usable=!settings.explicitly_disabled&&settings.enabled&&(oauth.resolve)(&settings).is_ok();
                        let credential=crate::native_bootstrap::bootstrap_native(oauth.store.as_ref(),usable).await;
                        if let Ok(Some(models))=crate::catalog_refresh::refresh_catalog(&agent_dir,&settings,credential.as_ref(),&executable,&environment,(oauth.now)() as f64,||async {(oauth.resolve)(&settings)}).await
                            && models.iter().map(|model|&model.id).ne(crate::models::static_models().iter().map(|model|&model.id)) && runtime.assert_active().is_ok() && shutdown.as_ref().is_none_or(|shutdown|!shutdown.signal().aborted()) {
                            config.models=Some(models);
                            if let Err(error)=runtime.register_provider(maho_ext_api::types::ProviderRegistration::Config {name:crate::oauth_login::PROVIDER_ID.into(),config:Box::new(config)},&path) {eprintln!("Cursor CLI catalog registration failed: {error}");}
                        }
                    };
                    if let Some(task)=catalog_refresh {
                        let mut task=task.lock().expect("catalog task");
                        if task.as_ref().is_none_or(tokio::task::JoinHandle::is_finished) {*task=Some(tokio::spawn(refresh));}
                    } else {
                        refresh.await;
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }));
        }
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
        let extension=CursorCliExtension {oauth,settings,stream:Arc::new(|_,_,_|panic!("registration never streams")),catalog:None,router:None,shutdown:None,catalog_refresh:None};
        let runtime=ExtensionRuntime::default();let mut api=ExtensionApi::new(LoadedExtension::new("cursor","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime.clone());extension.register(&mut api);
        let capture=Arc::new(Capture(Default::default()));runtime.bind_providers(capture.clone()).expect("bind");let registrations=capture.0.lock().expect("registrations");assert_eq!(registrations.len(),1);
        let ProviderRegistration::Config {name,config}=&registrations[0] else {panic!("config");};assert_eq!(name,crate::oauth_login::PROVIDER_ID);assert_eq!(config.models.as_ref().expect("offline models").len(),15);assert!(config.oauth.is_some());assert!(config.stream_simple.is_some());let eligible=config.fallback_eligible.as_ref().expect("fallback");assert!(eligible());disabled.store(true,std::sync::atomic::Ordering::SeqCst);assert!(!eligible());
    }
}
