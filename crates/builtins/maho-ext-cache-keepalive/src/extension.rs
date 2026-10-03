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
struct State {ctx:Option<ExtensionContext>,parked:bool,active:bool,generation:u64,attempts:u64,cost:f64,last:Option<i64>,messages:Vec<AgentMessage>,usage:Option<Usage>,work:Option<tokio::task::JoinHandle<()>>,retired:Vec<tokio::task::JoinHandle<()>>,entries:Vec<serde_json::Value>}
async fn retire(tasks:Vec<tokio::task::JoinHandle<()>>){for task in tasks{let _result=task.await;}}
fn append_entries(state:&Mutex<State>,sender:&ExtensionApi)->Result<(),ExtensionFailure>{
    let entries=std::mem::take(&mut state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entries);
    for entry in entries{sender.append_entry(CACHE_KEEPALIVE_ENTRY_TYPE,Some(entry))?;}
    Ok(())
}
fn stop(state:&mut State,reason:&str,force:bool){
    let append=state.active||state.work.is_some()||force;
    state.generation+=1;state.active=false;
    if let Some(work)=state.work.take(){work.abort();state.retired.push(work);}
    if append{state.entries.push(json!({"phase":"stopped","iterations":state.attempts,"stopReason":reason,"cumulativeEstimatedUsd":state.cost}));}
}
fn arm(state:Arc<Mutex<State>>,sender:Arc<ExtensionApi>,warm:WarmRequest)->Result<(),ExtensionFailure>{
    let (ctx,generation)={
        let current=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if current.parked||current.work.is_some(){return Ok(());}
        let Some(ctx)=current.ctx.clone()else{return Ok(())};(ctx,current.generation)
    };
    let Some(model)=ctx.model.clone()else{return Ok(())};
    let settings=ctx.get_prompt_cache_keep_alive_settings()?;
    if !settings.enabled||!is_warm_supported_model(&model){return Ok(());}
    let idle=ctx.is_idle();let pending=ctx.has_pending_messages()?;
    let Some(wait)=ctx.get_prompt_cache_safe_wait_seconds()?.filter(|wait|wait.is_finite())else{return Ok(())};
    let mut current=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if current.generation!=generation||current.parked||current.work.is_some(){return Ok(());}
    let Some(last)=current.last else{return Ok(())};
    let stopped=if !idle{Some(("agent-busy",false))}else if pending{Some(("pending-messages",false))}else if current.attempts>=settings.max_requests_per_session{Some(("max-requests",true))}else if current.cost+projected_ping_cost(&model,current.usage.as_ref())>settings.max_cost_usd_per_session.max(0.0){Some(("cost-cap",true))}else{None};
    if let Some((reason,force))=stopped{stop(&mut current,reason,force);drop(current);return append_entries(&state,&sender);}
    if !current.active{current.entries.push(json!({"phase":"started"}));current.active=true;}
    let delay=next_delay_ms(last as f64,wait,settings.margin_seconds,maho_ai::utils::diagnostics::now_ms() as f64);
    let task_state=state.clone();let task_sender=sender.clone();let next_warm=warm.clone();
    current.work=Some(tokio::spawn(async move{
        tokio::time::sleep(std::time::Duration::from_millis(delay as u64)).await;
        let current_ctx={let state=task_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.generation!=generation{return;}state.ctx.clone()};
        let Some(ctx)=current_ctx else{return;};let Some(model)=ctx.model.clone()else{return;};
        let readiness=(||->Result<_,ExtensionFailure>{Ok((ctx.get_prompt_cache_keep_alive_settings()?,ctx.is_idle(),ctx.has_pending_messages()?))})();
        let (settings,idle,pending)=match readiness{Ok(readiness)=>readiness,Err(error)=>{
            {let mut state=task_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.generation!=generation{return;}state.work=None;stop(&mut state,"provider-error",false);}
            if let Err(error)=append_entries(&task_state,&task_sender){ctx.ui.notify(&error.message,NotificationType::Error);}
            ctx.ui.notify(&error.message,NotificationType::Error);return;
        }};
        let request={
            let mut state=task_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.generation!=generation{return;}
            if !settings.enabled||!is_warm_supported_model(&model)||!idle||pending||state.attempts>=settings.max_requests_per_session||state.cost+projected_ping_cost(&model,state.usage.as_ref())>settings.max_cost_usd_per_session.max(0.0){state.work=None;drop(state);if let Err(error)=arm(task_state.clone(),task_sender.clone(),next_warm){ctx.ui.notify(&error.message,NotificationType::Error);}return;}
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
                    Some((json!({"iteration":request.3,"cachedTokens":usage.cache_read+usage.cache_write,"ttlSeconds":maho_ai::utils::prompt_cache_ttl::resolve_prompt_cache_ttl_seconds(&request.0,None).unwrap_or(0),"estimatedCostUsd":cost}),json!({"phase":"ping","iteration":request.3,"cacheRead":usage.cache_read,"cacheWrite":usage.cache_write,"estimatedCostUsd":cost,"cumulativeEstimatedUsd":state.cost})))
                }
                Ok(maho_ai::api::warm_prompt_cache::WarmPromptCacheResult::Unsupported)=>{stop(&mut state,"unsupported-model",false);None}
                Err(_)=>{stop(&mut state,"provider-error",false);None}
            }
        };
        if let Err(error)=append_entries(&task_state,&task_sender){request.2.ui.notify(&error.message,NotificationType::Error);return;}
        let Some(receipt)=receipt else{return;};
        task_sender.events.emit(CACHE_WARM_PING_EVENT,&receipt.0);
        if let Err(error)=task_sender.append_entry(CACHE_KEEPALIVE_ENTRY_TYPE,Some(receipt.1)){request.2.ui.notify(&error.message,NotificationType::Error);return;}
        if let Err(error)=arm(task_state,task_sender,next_warm){request.2.ui.notify(&error.message,NotificationType::Error);}
    }));
    drop(current);
    append_entries(&state,&sender)
}
impl Extension for CacheKeepalive{
    fn register(&self,api:&mut ExtensionApi){
        let state=Arc::new(Mutex::new(State{ctx:None,parked:false,active:false,generation:0,attempts:0,cost:0.0,last:None,messages:vec![],usage:None,work:None,retired:vec![],entries:vec![]}));
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
                let restored=if kind==EventKind::SessionStart{
                    let entries=ctx.session_manager.get_entries().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
                    let snapshot=maho_core::session_manager::build_session_context(&entries,ctx.session_manager.get_leaf_id().as_deref());
                    Some(snapshot.messages.into_iter().map(|value|serde_json::from_value::<AgentMessage>(value).map_err(|error|ExtensionFailure::new(error.to_string()))).collect::<Result<Vec<_>,_>>())
                }else{None};
                let retirement;
                let mut rearm=matches!(kind,EventKind::SessionStart|EventKind::AgentEnd|EventKind::ModelSelect|EventKind::SessionResumed);
                let transition;
                {
                    let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    transition=(||->Result<(),ExtensionFailure>{
                    match event{
                        ExtensionEvent::SessionStart(_)=>{
                            stop(&mut state,"session-restart",false);state.ctx=Some(ctx.clone());state.attempts=0;state.cost=0.0;
                            state.messages=restored.expect("session-start restoration")?;
                            state.usage=last_assistant_usage(&state.messages).map(|(usage,_)|usage.clone());state.last=last_assistant_timestamp(&state.messages);
                        }
                        ExtensionEvent::AgentEnd{messages,..}=>{state.ctx=Some(ctx.clone());if last_assistant_usage(messages).is_some_and(|(_,reason)|reason==maho_ai::types::StopReason::Error){rearm=false;stop(&mut state,"provider-error",false);}else{state.messages=messages.clone();state.usage=last_assistant_usage(messages).map(|(usage,_)|usage.clone());state.last=Some(maho_ai::utils::diagnostics::now_ms());}}
                        ExtensionEvent::ModelSelect(_)=>{state.ctx=Some(ctx.clone());stop(&mut state,"model-changed",false);}
                        ExtensionEvent::SessionParked=>{state.parked=true;stop(&mut state,"session-parked",false);}
                        ExtensionEvent::SessionResumed=>{state.parked=false;state.ctx=Some(ctx.clone());}
                        ExtensionEvent::AgentStart=>{stop(&mut state,"agent-busy",false);}
                        ExtensionEvent::Input(_)=>{stop(&mut state,"user-input",false);}
                        ExtensionEvent::SessionShutdown(_)=>{stop(&mut state,"session-dispose",false);state.ctx=None;}
                        _=>{}
                    }
                    Ok(())})();
                    retirement=std::mem::take(&mut state.retired);
                }
                retire(retirement).await;
                append_entries(&state,&sender)?;
                transition?;
                if rearm{arm(state,sender,warm)?;}
                Ok(EventResult::None)
            })}));
        }
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    struct Dropped(tokio::sync::oneshot::Sender<()>);
    impl Drop for Dropped{fn drop(&mut self){let (replacement,_)=tokio::sync::oneshot::channel();let sender=std::mem::replace(&mut self.0,replacement);let _result=sender.send(());}}
    #[tokio::test]
    async fn retirement_waits_for_cancelled_provider_future_drop(){
        let (entered,started)=tokio::sync::oneshot::channel();
        let (dropped,finished)=tokio::sync::oneshot::channel();
        let task=tokio::spawn(async move{let _owned=Dropped(dropped);let _result=entered.send(());std::future::pending::<()>().await;});
        tokio::time::timeout(std::time::Duration::from_secs(5),started).await.expect("provider entered").expect("signal");
        task.abort();
        tokio::time::timeout(std::time::Duration::from_secs(5),retire(vec![task])).await.expect("retirement");
        assert!(finished.await.is_ok());
    }
}
