use std::{collections::BTreeMap,path::PathBuf,sync::{Arc,Mutex}};
use maho_ext_api::{EventKind,EventResult,ExtensionApi,ExtensionContext,ExtensionFailure,ExtensionUi,NotificationType};
use crate::{context::MemoryIdentityContext,binding::{MEMORY_BINDING_CUSTOM_TYPE,create_memory_binding},supervisor::MemoryModuleSupervisor};
pub struct MemoryComponentOptions{
    pub env:BTreeMap<String,String>,pub load_config:Arc<dyn Fn()->Result<serde_json::Value,String>+Send+Sync>,
    pub cwd:PathBuf,pub now:Arc<dyn Fn()->f64+Send+Sync>,pub disabled:Arc<dyn Fn()->bool+Send+Sync>,
}
pub struct MemorySessionState{pub enabled:bool,pub context:Option<MemoryIdentityContext>,pub memory_status_attempted:bool,pub restart_notified:bool,ui:Option<Arc<dyn ExtensionUi>>}
pub struct MemoryComponent{options:MemoryComponentOptions,pub sessions:Mutex<BTreeMap<String,MemorySessionState>>,pub supervisor:MemoryModuleSupervisor}
pub type MemoryHookFuture<'a>=std::pin::Pin<Box<dyn std::future::Future<Output=Result<(),String>>+Send+'a>>;
pub type MemoryBindHook=Arc<dyn for<'a> Fn(&'a str,&'a MemoryIdentityContext,&'a ExtensionContext)->MemoryHookFuture<'a>+Send+Sync>;
pub type MemoryShutdownHook=Arc<dyn for<'a> Fn(&'a maho_ext_api::ExtensionEvent,&'a ExtensionContext,f64)->MemoryHookFuture<'a>+Send+Sync>;
pub struct MemoryComponentHooks{pub after_bind:MemoryBindHook,pub shutdown:MemoryShutdownHook,pub clear_status:Arc<dyn Fn(&ExtensionContext)+Send+Sync>,pub warn:Arc<dyn Fn(&str)+Send+Sync>}
pub fn is_memory_child_process(env:&BTreeMap<String,String>)->bool{["SENPI_MEMORY_REFLECTION","SENPI_MEMORY_FACTS"].iter().any(|key|env.get(*key).is_some_and(|value|value=="1"))}
impl MemoryComponent{
    pub fn new(options:MemoryComponentOptions)->Arc<Self>{Arc::new(Self{options,sessions:Mutex::new(BTreeMap::new()),supervisor:Default::default()})}
    fn settings(&self)->Result<serde_json::Value,String>{let config=(self.options.load_config)()?;crate::reflection_settings::resolve_memory_settings(config.get("memory"))}
    fn enabled(&self,settings:&serde_json::Value)->bool{settings["enabled"]==true&&!is_memory_child_process(&self.options.env)&&!(self.options.disabled)()}
    fn release(&self,state:&mut MemorySessionState){if state.context.take().is_some(){self.supervisor.release();}}
    pub fn register(self:&Arc<Self>,api:&mut ExtensionApi,hooks:MemoryComponentHooks,register_static:impl FnOnce(&mut ExtensionApi))->Result<bool,String>{
        let boot=self.settings()?;if !self.enabled(&boot){return Ok(false);}
        if !crate::capabilities::has_memory_capabilities(api){return Ok(false);}
        register_static(api);
        let this=self.clone();let subscription=Arc::new(Mutex::new(Some(api.events.on("config-watch:reloaded",Arc::new(move |payload|{
            if payload["registrationId"]!="omo"{return;}
            let Ok(settings)=this.settings()else{return;};let enabled=this.enabled(&settings);
            for state in this.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values_mut(){if state.enabled!=enabled&&!state.restart_notified{state.restart_notified=true;if let Some(ui)=&state.ui{ui.notify("restart required to apply memory config change",NotificationType::Warning);}}}
        })))));
        let this=self.clone();let handle=ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone());let clear=hooks.clear_status.clone();let bind=hooks.after_bind;let warn=hooks.warn;
        api.on(EventKind::SessionStart,Arc::new(move |_,context|{
            clear(context);let result=(||{
                let settings=this.settings()?;let enabled=this.enabled(&settings);let id=context.session_manager.session_id();let id=if id.is_empty(){"unknown-session"}else{id};
                let mut sessions=this.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(previous)=sessions.get_mut(id){this.release(previous);}
                sessions.insert(id.into(),MemorySessionState{enabled,context:None,memory_status_attempted:false,restart_notified:false,ui:Some(context.ui.clone())});
                if !enabled{return Ok(None);}
                let identity=memory_core::identity::resolve::resolve_memory_identity(settings["agent"].as_str(),&this.options.cwd,&this.options.env).map_err(|error|error.to_string())?;
                let entries=context.session_manager.get_entries();
                let previous=entries.iter().rev().find_map(|entry|{let data=&entry.data;if entry.kind=="custom"&&data["customType"]==MEMORY_BINDING_CUSTOM_TYPE{data["data"]["identity"].as_str()}else{None}});
                if let Some(previous)=previous&&previous!=identity.id{context.ui.notify(&format!("memory identity conflict: session is bound to {previous}, but config resolved {}; restart with the original identity or fork a new session",identity.id),NotificationType::Error);return Ok(None);}
                let binding=create_memory_binding(&identity.id,&identity.paths.repo.to_string_lossy(),(this.options.now)());let value=serde_json::json!({"identity":binding.identity,"repoPathHash":binding.repo_path_hash,"boundAt":binding.bound_at});
                let bound=MemoryIdentityContext::new(identity.id,identity.paths,binding);
                let state=sessions.get_mut(id).ok_or("Inserted memory session is missing")?;state.context=Some(bound.clone());this.supervisor.acquire();drop(sessions);
                handle.append_entry(MEMORY_BINDING_CUSTOM_TYPE,Some(value)).map_err(|error|error.to_string())?;
                Ok::<_,String>(Some((id.to_owned(),bound)))
            })();let bind=bind.clone();let warn=warn.clone();Box::pin(async move{
                if let Some((id,bound))=result.map_err(ExtensionFailure::new)?&&let Err(error)=bind(&id,&bound,context).await{warn(&format!("memory bind-time reconcile failed: {error}"));}
                Ok(EventResult::None)
            })
        }));
        let this=self.clone();api.on(EventKind::SessionShutdown,Arc::new(move |event,context|{
            let deadline=crate::shutdown_drain::shutdown_deadline_at(||(this.options.now)());let this=this.clone();let shutdown=hooks.shutdown.clone();let clear=hooks.clear_status.clone();let subscription=subscription.clone();
            Box::pin(async move{
                let result=shutdown(event,context,deadline).await;
                clear(context);if let Some(mut state)=this.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(context.session_manager.session_id()){this.release(&mut state);}
                drop(subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take());
                result.map(|_|EventResult::None).map_err(ExtensionFailure::new)
            })
        }));Ok(true)
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn only_exact_child_sentinels_disable_memory(){for key in ["SENPI_MEMORY_REFLECTION","SENPI_MEMORY_FACTS"]{assert!(is_memory_child_process(&BTreeMap::from([(key.into(),"1".into())])));assert!(!is_memory_child_process(&BTreeMap::from([(key.into(),"true".into())])));}}
    #[test]fn disabled_boot_loads_once_without_registering(){let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));let seen=calls.clone();let component=MemoryComponent::new(MemoryComponentOptions{env:Default::default(),cwd:"/project".into(),now:Arc::new(||0.0),disabled:Arc::new(||true),load_config:Arc::new(move||{seen.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(serde_json::json!({}))})});let mut api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());assert!(!component.register(&mut api,MemoryComponentHooks{after_bind:Arc::new(|_,_,_|panic!("disabled")),shutdown:Arc::new(|_,_,_|panic!("disabled")),clear_status:Arc::new(|_|panic!("disabled")),warn:Arc::new(|_|panic!("disabled"))},|_|panic!("disabled")).unwrap());assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),1);assert!(api.registered.handlers.is_empty());}
}
