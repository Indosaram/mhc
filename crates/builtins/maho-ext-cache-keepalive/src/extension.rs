use crate::*;
use maho_ext_api::*;
use maho_agent::types::AgentMessage;
use serde_json::json;
use std::sync::{Arc,Mutex};

pub type WarmFuture = std::pin::Pin<Box<dyn std::future::Future<Output=Result<maho_ai::api::warm_prompt_cache::WarmPromptCacheResult,ExtensionFailure>>+Send>>;
pub type WarmRequest = Arc<dyn Fn(Model,Vec<AgentMessage>,ExtensionContext,Arc<ExtensionApi>)->WarmFuture+Send+Sync>;
pub struct CacheKeepalive { pub warm: WarmRequest }
impl Default for CacheKeepalive{
    fn default()->Self{Self{warm:Arc::new(|model,messages,ctx,sender|Box::pin(async move{
        let prepared=ctx.prepare_provider_request(messages).await?;
        let active=sender.runtime.session_actions()?.get_active_tools()?;
        let tools=sender.get_all_tools()?.into_iter().filter(|tool|active.contains(&tool.name)).map(|tool|maho_ai::types::Tool{name:tool.name,description:tool.description,parameters:tool.parameters}).collect::<Vec<_>>();
        let raw=prepared.messages.iter().map(|message|serde_json::to_value(message).map_err(|error|ExtensionFailure::new(error.to_string()))).collect::<Result<Vec<_>,_>>()?;
        let messages=maho_core::messages::convert_to_llm(&maho_core::messages::filter_context_excluded_messages(raw)).into_iter().map(|message|serde_json::from_value(message).map_err(|error|ExtensionFailure::new(error.to_string()))).collect::<Result<Vec<_>,_>>()?;
        let context=maho_ai::types::Context{system_prompt:Some(ctx.get_system_prompt()),messages,tools:(!tools.is_empty()).then_some(tools)};
        let auth=ctx.model_registry.get_api_key_and_headers(&model).await?;
        let mut options=maho_ai::types::StreamOptions::default();
        options.request.api_key=auth.auth.api_key;options.request.headers=Some((prepared.transform_headers)(auth.auth.headers.unwrap_or_default()).await?);options.request.env=auth.env;options.request.affinity_session_id=Some(ctx.session_manager.session_id().into());options.cache_retention=model.cache_retention;
        let transform=prepared.transform_payload;
        options.request.async_on_payload=Some(Arc::new(move|payload,_,_|{let transform=transform.clone();Box::pin(async move{transform(payload).await.map(Some).map_err(|error|error.to_string())})}));
        maho_ai::api::warm_prompt_cache::warm_prompt_cache(&model,&context,Some(options),&|model,context,options|Ok(serde_json::Value::Object(maho_ai::api::anthropic_messages::build_anthropic_warm_prompt_cache_params(model,context,Some(options))))).await.map_err(ExtensionFailure::new)
    }))}}
}
struct State {ctx:Option<ExtensionContext>,parked:bool,active:bool,generation:u64,attempts:u64,cost:f64,last:Option<i64>,messages:Vec<AgentMessage>,usage:Option<Usage>,work:Option<tokio::task::JoinHandle<()>>,retired:Vec<tokio::task::JoinHandle<()>>}
fn stop(state:&mut State,sender:&ExtensionApi,reason:&str,force:bool)->Result<(),ExtensionFailure>{
    let append=state.active||state.work.is_some()||force;
    state.generation+=1;state.active=false;
    if let Some(work)=state.work.take(){work.abort();state.retired.push(work);}
    if append{sender.append_entry(CACHE_KEEPALIVE_ENTRY_TYPE,Some(json!({"phase":"stopped","iterations":state.attempts,"stopReason":reason,"cumulativeEstimatedUsd":state.cost})))?;}
    Ok(())
}
fn arm(state:Arc<Mutex<State>>,sender:Arc<ExtensionApi>,warm:WarmRequest)->Result<(),ExtensionFailure>{
    let mut current=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if current.parked||current.work.is_some(){return Ok(());}
    let Some(ctx)=current.ctx.clone()else{return Ok(())};let Some(model)=ctx.model.clone()else{return Ok(())};
    let settings=ctx.get_prompt_cache_keep_alive_settings()?;
    let Some(last)=current.last else{return Ok(())};
    if !settings.enabled||!is_warm_supported_model(&model){return Ok(());}
    if !ctx.is_idle(){return stop(&mut current,&sender,"agent-busy",false);}
    if ctx.has_pending_messages()?{return stop(&mut current,&sender,"pending-messages",false);}
    let Some(wait)=ctx.get_prompt_cache_safe_wait_seconds()?.filter(|wait|wait.is_finite())else{return Ok(())};
    if current.attempts>=settings.max_requests_per_session{return stop(&mut current,&sender,"max-requests",true);}
    if current.cost+projected_ping_cost(&model,current.usage.as_ref())>settings.max_cost_usd_per_session.max(0.0){return stop(&mut current,&sender,"cost-cap",true);}
    if !current.active{sender.append_entry(CACHE_KEEPALIVE_ENTRY_TYPE,Some(json!({"phase":"started"})))?;current.active=true;}
    let delay=next_delay_ms(last as f64,wait,settings.margin_seconds,maho_ai::utils::diagnostics::now_ms() as f64);
    let generation=current.generation;
    let task_state=state.clone();let task_sender=sender.clone();let next_warm=warm.clone();
    current.work=Some(tokio::spawn(async move{
        tokio::time::sleep(std::time::Duration::from_millis(delay as u64)).await;
        let request={
            let mut state=task_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.generation!=generation{return;}
            let Some(ctx)=state.ctx.clone()else{return;};let Some(model)=ctx.model.clone()else{return;};
            let settings=match ctx.get_prompt_cache_keep_alive_settings(){Ok(settings)=>settings,Err(_)=>return};
            if !settings.enabled||!is_warm_supported_model(&model)||!ctx.is_idle()||ctx.has_pending_messages().unwrap_or(true)||state.attempts>=settings.max_requests_per_session||state.cost+projected_ping_cost(&model,state.usage.as_ref())>settings.max_cost_usd_per_session.max(0.0){state.work=None;drop(state);let _result=arm(task_state.clone(),task_sender.clone(),next_warm);return;}
            state.attempts+=1;(model,state.messages.clone(),ctx,state.attempts)
        };
        let result=warm(request.0.clone(),request.1,request.2.clone(),task_sender.clone()).await;
        let receipt={
            let mut state=task_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.generation!=generation{return;}
            state.work=None;
            match result{
                Ok(maho_ai::api::warm_prompt_cache::WarmPromptCacheResult::Supported{usage,..})=>{
                    let usage=Usage{input:usage.input,output:usage.output,cache_read:usage.cache_read,cache_write:usage.cache_write,..Default::default()};
                    let cost=actual_ping_cost(&request.0,&usage);state.cost+=cost;state.last=Some(maho_ai::utils::diagnostics::now_ms());
                    (json!({"iteration":request.3,"cachedTokens":usage.cache_read+usage.cache_write,"ttlSeconds":maho_ai::utils::prompt_cache_ttl::resolve_prompt_cache_ttl_seconds(&request.0,None).unwrap_or(0),"estimatedCostUsd":cost}),json!({"phase":"ping","iteration":request.3,"cacheRead":usage.cache_read,"cacheWrite":usage.cache_write,"estimatedCostUsd":cost,"cumulativeEstimatedUsd":state.cost}))
                }
                Ok(maho_ai::api::warm_prompt_cache::WarmPromptCacheResult::Unsupported)=>{let _result=stop(&mut state,&task_sender,"unsupported-model",false);return;}
                Err(_)=>{let _result=stop(&mut state,&task_sender,"provider-error",false);return;}
            }
        };
        task_sender.events.emit(CACHE_WARM_PING_EVENT,&receipt.0);
        if let Err(error)=task_sender.append_entry(CACHE_KEEPALIVE_ENTRY_TYPE,Some(receipt.1)){request.2.ui.notify(&error.message,NotificationType::Error);return;}
        let _result=arm(task_state,task_sender,next_warm);
    }));
    Ok(())
}
impl Extension for CacheKeepalive{
    fn register(&self,api:&mut ExtensionApi){
        let state=Arc::new(Mutex::new(State{ctx:None,parked:false,active:false,generation:0,attempts:0,cost:0.0,last:None,messages:vec![],usage:None,work:None,retired:vec![]}));
        let sender=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        api.register_entry_renderer(CACHE_KEEPALIVE_ENTRY_TYPE,Arc::new(|entry,options,_|{
            let data=&entry.data["data"];if data["phase"]!="ping"{return None;}
            let tokens=data["cacheRead"].as_u64().unwrap_or(0)+data["cacheWrite"].as_u64().unwrap_or(0);
            let mut text=format!("Warm ping #{} - {tokens} tokens refreshed - ${:.3}",data["iteration"],data["estimatedCostUsd"].as_f64().unwrap_or(0.0));
            if options.expanded{text.push_str(&format!("\ncache read {} - cache write {} - session total ${:.3}",data["cacheRead"],data["cacheWrite"],data["cumulativeEstimatedUsd"].as_f64().unwrap_or(0.0)));}
            Some(Box::new(maho_tui::components::text::Text::new(text)))
        }),Default::default());
        for kind in [EventKind::SessionStart,EventKind::AgentEnd,EventKind::ModelSelect,EventKind::SessionParked,EventKind::SessionResumed,EventKind::AgentStart,EventKind::Input,EventKind::SessionShutdown]{
            let state=state.clone();let sender=sender.clone();let warm=self.warm.clone();
            api.on(kind,Arc::new(move|event,ctx|{let state=state.clone();let sender=sender.clone();let warm=warm.clone();Box::pin(async move{
                let retirement;
                {
                    let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    match event{
                        ExtensionEvent::SessionStart(_)=>{
                            stop(&mut state,&sender,"session-restart",false)?;state.ctx=Some(ctx.clone());state.attempts=0;state.cost=0.0;
                            let entries=ctx.session_manager.get_entries().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
                            let snapshot=maho_core::session_manager::build_session_context(&entries,ctx.session_manager.get_leaf_id().as_deref());
                            state.messages=snapshot.messages.into_iter().map(|value|serde_json::from_value(value).map_err(|error|ExtensionFailure::new(error.to_string()))).collect::<Result<Vec<_>,_>>()?;
                            state.usage=last_assistant_usage(&state.messages).map(|(usage,_)|usage.clone());state.last=last_assistant_timestamp(&state.messages);
                        }
                        ExtensionEvent::AgentEnd{messages,..}=>{state.ctx=Some(ctx.clone());if last_assistant_usage(messages).is_some_and(|(_,reason)|reason==maho_ai::types::StopReason::Error){stop(&mut state,&sender,"provider-error",false)?;return Ok(EventResult::None);}state.messages=messages.clone();state.usage=last_assistant_usage(messages).map(|(usage,_)|usage.clone());state.last=Some(maho_ai::utils::diagnostics::now_ms());}
                        ExtensionEvent::ModelSelect(_)=>{state.ctx=Some(ctx.clone());stop(&mut state,&sender,"model-changed",false)?;}
                        ExtensionEvent::SessionParked=>{state.parked=true;stop(&mut state,&sender,"session-parked",false)?;}
                        ExtensionEvent::SessionResumed=>{state.parked=false;state.ctx=Some(ctx.clone());}
                        ExtensionEvent::AgentStart=>{stop(&mut state,&sender,"agent-busy",false)?;}
                        ExtensionEvent::Input(_)=>{stop(&mut state,&sender,"user-input",false)?;}
                        ExtensionEvent::SessionShutdown(_)=>{stop(&mut state,&sender,"session-dispose",false)?;state.ctx=None;}
                        _=>{}
                    }
                    retirement=std::mem::take(&mut state.retired);
                }
                for work in retirement{let _result=work.await;}
                if matches!(kind,EventKind::SessionStart|EventKind::AgentEnd|EventKind::ModelSelect|EventKind::SessionResumed){arm(state,sender,warm)?;}
                Ok(EventResult::None)
            })}));
        }
    }
}
