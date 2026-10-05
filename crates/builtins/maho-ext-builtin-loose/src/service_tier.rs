use serde_json::Value;
use maho_ext_api::ServiceTier;
pub const CODEX_RESPONSES_API:&str="openai-codex-responses";
pub fn supports_service_tier(api:Option<&str>)->bool{matches!(api,Some("openai-responses"|CODEX_RESPONSES_API))}
pub fn add_service_tier_to_payload(api:Option<&str>,mut payload:Value,tier:Option<ServiceTier>)->Value{
    if supports_service_tier(api)&&let Some(tier)=tier&&let Some(object)=payload.as_object_mut()&&!object.contains_key("service_tier"){
        object.insert("service_tier".into(),Value::String(match tier{ServiceTier::Auto=>"auto",ServiceTier::Flex=>"flex",ServiceTier::Priority=>"priority"}.into()));
    }
    payload
}
pub fn effective_service_tier(api:Option<&str>,session_fast_mode:bool,context_tier:Option<ServiceTier>,settings_tier:Option<ServiceTier>,live_memory_tier:Option<ServiceTier>,same_base_key:bool,catalog_explains_priority:bool)->Option<ServiceTier>{
    if api==Some(CODEX_RESPONSES_API){
        if session_fast_mode{Some(ServiceTier::Priority)}else if live_memory_tier==Some(ServiceTier::Auto)&&same_base_key&&context_tier==Some(ServiceTier::Priority)&&catalog_explains_priority{None}else{context_tier}
    }else{context_tier.or(settings_tier)}
}

use maho_ext_api::*;
use std::sync::{Arc,Mutex};
pub trait ServiceTierHost:Send+Sync{
    fn catalog_tier(&self,model:&Model)->Option<ServiceTier>;
    fn upstream_id(&self,model:&Model)->String;
    fn remembered(&self,ctx:&ExtensionContext,model:&Model)->Result<Option<ServiceTier>,ExtensionFailure>;
    fn global_tier(&self,ctx:&ExtensionContext)->Result<Option<ServiceTier>,ExtensionFailure>;
    fn persist<'a>(&'a self,ctx:&'a ExtensionContext,model:&'a Model,tier:ServiceTier)->ExtensionFuture<'a,()>;
    fn thinking_level(&self,ctx:&ExtensionContext)->Result<Option<maho_ai::types::ModelThinkingLevel>,ExtensionFailure>;
    fn restore_thinking_level(&self,ctx:&ExtensionContext,level:maho_ai::types::ModelThinkingLevel)->Result<(),ExtensionFailure>;
}
fn sibling(ctx:&ExtensionContext,host:&dyn ServiceTierHost,model:&Model,fast:bool)->Option<Model>{
    let id=if fast{if model.id.ends_with("-fast"){return None;}format!("{}-fast",model.id)}else{model.id.strip_suffix("-fast")?.into()};
    let sibling=ctx.model_registry.find(&model.provider,&id)?;
    let priority=if fast{&sibling}else{model};
    (sibling.provider==model.provider&&sibling.api==model.api&&host.catalog_tier(priority)==Some(ServiceTier::Priority)&&host.upstream_id(&sibling)==host.upstream_id(model)).then_some(sibling)
}
fn memory_model(ctx:&ExtensionContext,host:&dyn ServiceTierHost,model:&Model)->Model{sibling(ctx,host,model,false).unwrap_or_else(||model.clone())}
fn key(model:&Model)->String{format!("{}/{}",model.provider,model.id)}
pub struct FastModeResult{pub enabled:bool,pub applied:bool,pub recorded_tier:ServiceTier}
pub async fn apply_fast_mode(sender:&ExtensionApi,ctx:&ExtensionContext,host:&dyn ServiceTierHost,enabled:bool)->Result<FastModeResult,ExtensionFailure>{
    let Some(model)=ctx.model.as_ref().filter(|model|model.api==CODEX_RESPONSES_API)else{
        ctx.ui.notify("Fast mode is only available for ChatGPT Subscription models.",NotificationType::Warning);
        return Ok(FastModeResult{enabled:false,applied:false,recorded_tier:ServiceTier::Auto});
    };
    if !enabled&&ctx.service_tier==Some(ServiceTier::Priority)&&host.catalog_tier(model)!=Some(ServiceTier::Priority){
        ctx.ui.notify("Fast mode is fixed by the active model selection's priority tier.",NotificationType::Info);
        return Ok(FastModeResult{enabled:true,applied:false,recorded_tier:ServiceTier::Priority});
    }
    let memory=memory_model(ctx,host,model);let tier=if enabled{ServiceTier::Priority}else{ServiceTier::Auto};host.persist(ctx,&memory,tier).await?;
    let base=sibling(ctx,host,model,false);let target=if enabled{sibling(ctx,host,model,true)}else{base.clone()};
    if let Some(target)=&target&& !sender.set_session_model(target.clone()).await?{
        ctx.ui.notify(&format!("Could not switch to {}.",key(target)),NotificationType::Error);
        return Ok(FastModeResult{enabled:base.is_none(),applied:false,recorded_tier:ServiceTier::Auto});
    }
    sender.set_session_fast_mode(enabled)?;
    ctx.ui.notify(&format!("Fast mode {}: {}",if enabled{"enabled"}else{"disabled"},target.as_ref().unwrap_or(model).id),NotificationType::Info);
    Ok(FastModeResult{enabled,applied:true,recorded_tier:tier})
}
#[derive(Default)]
struct State{fast:bool,global:Option<ServiceTier>,memory:Option<ServiceTier>,key:Option<String>}
pub struct ServiceTierExtension{pub host:Arc<dyn ServiceTierHost>}
impl Extension for ServiceTierExtension{
    fn register(&self,api:&mut ExtensionApi){
        let sender=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));let state=Arc::new(Mutex::new(State::default()));
        for kind in [EventKind::SessionStart,EventKind::ModelSelect,EventKind::BeforeProviderRequest]{let host=self.host.clone();let sender=sender.clone();let state=state.clone();api.on(kind,Arc::new(move|event,ctx|{let host=host.clone();let sender=sender.clone();let state=state.clone();Box::pin(async move{
            if let ExtensionEvent::BeforeProviderRequest{payload,..}=event{
                let model=ctx.model.as_ref();let memory=model.map(|model|memory_model(ctx,host.as_ref(),model));let catalog=model.and_then(|model|host.catalog_tier(model));
                let state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                return Ok(EventResult::ProviderPayload(add_service_tier_to_payload(model.map(|model|model.api.as_str()),payload.clone(),effective_service_tier(model.map(|model|model.api.as_str()),state.fast,ctx.service_tier,state.global,state.memory,memory.as_ref().map(key)==state.key,catalog==Some(ServiceTier::Priority)))));
            }
            let model=match event{ExtensionEvent::ModelSelect(event)=>Some(&event.model),_=>ctx.model.as_ref()};
            let memory=model.map(|model|memory_model(ctx,host.as_ref(),model));let remembered=memory.as_ref().map(|model|host.remembered(ctx,model)).transpose()?.flatten();
            if kind==EventKind::SessionStart{
                let global=host.global_tier(ctx)?;
                let mut fast=false;
                if let Some(model)=model.filter(|model|model.api==CODEX_RESPONSES_API){
                    let base=sibling(ctx,host.as_ref(),model,false);let requested=host.thinking_level(ctx)?;
                    if let Some(base)=&base{sender.set_session_model(base.clone()).await?;if let Some(requested)=requested{host.restore_thinking_level(ctx,requested)?;}}
                    fast=base.is_some()||remembered==Some(ServiceTier::Priority)||(remembered.is_none()&&ctx.service_tier==Some(ServiceTier::Priority));
                }
                {let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.global=global;state.fast=fast;state.memory=remembered;state.key=memory.as_ref().map(key);}
                sender.set_session_fast_mode(fast)?;
            }else{
                let catalog_priority=model.is_some_and(|model|model.api==CODEX_RESPONSES_API&&host.catalog_tier(model)==Some(ServiceTier::Priority));
                let clear={let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.memory=remembered;state.key=memory.as_ref().map(key);if state.fast&&model.is_none_or(|model|model.api!=CODEX_RESPONSES_API){state.fast=false;true}else{!state.fast&&remembered==Some(ServiceTier::Auto)&&catalog_priority}};
                if clear{sender.set_session_fast_mode(false)?;}
            }
            Ok(EventResult::None)
        })}));}
        let host=self.host.clone();
        api.register_command_with_completions("fast",Some("Turn ChatGPT Subscription fast mode on or off for the current model".into()),Some("[on|off]".into()),Arc::new(move|args,ctx|{let host=host.clone();let sender=sender.clone();let state=state.clone();Box::pin(async move{
            let argument=args.trim().to_ascii_lowercase();if !matches!(argument.as_str(),""|"on"|"off"){ctx.ui.notify("Usage: /fast [on|off]",NotificationType::Error);return Ok(());}
            let active=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).fast||ctx.service_tier==Some(ServiceTier::Priority);let enabled=if argument.is_empty(){!active}else{argument=="on"};
            let result=apply_fast_mode(&sender,ctx,host.as_ref(),enabled).await?;
            let memory=if result.applied{ctx.model.as_ref().map(|model|memory_model(ctx,host.as_ref(),model))}else{None};
            {let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.fast=result.enabled;if let Some(memory)=memory{state.memory=Some(result.recorded_tier);state.key=Some(key(&memory));}}
            Ok(())
        })}),Arc::new(|prefix|Box::pin(async move{let items=["on","off"].into_iter().filter(|value|value.starts_with(prefix.trim())).map(|value|AutocompleteItem{value:value.into(),label:value.into(),description:None}).collect::<Vec<_>>();Ok((!items.is_empty()).then_some(items))})));
    }
}
