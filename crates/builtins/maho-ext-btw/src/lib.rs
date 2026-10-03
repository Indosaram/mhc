pub mod side_query;
pub mod panel;

use maho_ext_api::*;
use std::sync::{Arc,Mutex};
struct Active {id:u64,controller:maho_ai::utils::abort::AbortController,settled:bool,unsubscribe:Option<UiUnsubscribe>}
struct State {next:u64,active:Option<Active>}
fn dismiss(state:&Mutex<State>,ctx:&ExtensionContext,abort:bool){
    let active=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.take();
    if let Some(active)=active{if abort{active.controller.abort(None);}if let Some(unsubscribe)=active.unsubscribe{unsubscribe();}ctx.ui.set_widget("btw",None,Default::default());}
}
pub struct Btw;
impl Extension for Btw{
    fn register(&self,api:&mut ExtensionApi){
        let state=Arc::new(Mutex::new(State{next:0,active:None}));
        for kind in [EventKind::SessionBeforeSwitch,EventKind::SessionBeforeFork,EventKind::SessionShutdown,EventKind::Input]{
            let state=state.clone();api.on(kind,Arc::new(move|_,ctx|{let state=state.clone();Box::pin(async move{let close=kind!=EventKind::Input||state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().is_some_and(|active|active.settled);if close{dismiss(&state,ctx,kind!=EventKind::Input);}Ok(EventResult::None)})}));
        }
        let runtime=api.runtime.clone();
        api.register_command("btw",Some("Ask a side question in parallel without touching the main session".into()),Some("<question>".into()),Arc::new(move|args,ctx|{let state=state.clone();let runtime=runtime.clone();Box::pin(async move{
            let question=args.trim();
            if question.is_empty(){if state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.is_some(){dismiss(&state,ctx,true);}else{ctx.ui.notify("Usage: /btw <question>",NotificationType::Warning);}return Ok(());}
            let Some(model)=ctx.model.clone()else{ctx.ui.notify("No active model available for /btw.",NotificationType::Error);return Ok(())};
            let entries=ctx.session_manager.get_entries().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
            let snapshot=maho_core::session_manager::build_session_context(&entries,ctx.session_manager.get_leaf_id().as_deref());
            let history=maho_core::messages::convert_to_llm(&maho_core::messages::filter_context_excluded_messages(snapshot.messages)).into_iter().map(|message|serde_json::from_value(message).map_err(|error|ExtensionFailure::new(error.to_string()))).collect::<Result<Vec<_>,_>>()?;
            let context=side_query::build_side_query_context(&ctx.get_system_prompt(),history,question,&model).map_err(ExtensionFailure::new)?;
            dismiss(&state,ctx,true);
            let controller=maho_ai::utils::abort::AbortController::new();let signal=controller.signal();
            let id={let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.next+=1;let id=state.next;state.active=Some(Active{id,controller,settled:false,unsubscribe:None});id};
            if ctx.mode==ExtensionMode::Tui&&ctx.has_ui{
                ctx.ui.set_widget("btw",Some(panel::widget(question,"",false)),Default::default());
                let callback=state.clone();let owner=ctx.clone();
                let subscription=ctx.ui.on_terminal_input(Arc::new(move|data|{if !maho_tui::keys::is_key_release(data)&&maho_tui::keys::matches_key(data,"escape")&&callback.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().is_some_and(|active|active.id==id){dismiss(&callback,&owner,true);}None}));
                let unsubscribe=match subscription{Ok(unsubscribe)=>unsubscribe,Err(error)=>{dismiss(&state,ctx,true);return Err(error);}};
                let mut unsubscribe=Some(unsubscribe);
                {let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(active)=state.active.as_mut().filter(|active|active.id==id){active.unsubscribe=unsubscribe.take();}}
                if let Some(unsubscribe)=unsubscribe{unsubscribe();}
            }
            let outcome=async{
                let auth=ctx.model_registry.get_api_key_and_headers(&model).await?;
                let mut options=maho_ai::types::SimpleStreamOptions::default();options.stream.request.api_key=auth.auth.api_key;options.stream.request.headers=auth.auth.headers;options.stream.request.env=auth.env;options.stream.request.signal=Some(signal.clone());options.stream.request.affinity_session_id=Some(format!("{}:btw:{id}",ctx.session_manager.session_id()));options.stream.extra=auth.extra_body.unwrap_or_default();options.reasoning=Some(runtime.session_actions()?.get_thinking_level()?);
                let stream=ctx.model_registry.stream_simple(&model,&context,Some(options))?;
                let owner=ctx.clone();let mut reply=String::new();
                let collected=side_query::collect_reply(&stream,side_query::DEFAULT_ESTABLISHMENT_TIMEOUT_MS,|delta|{reply.push_str(delta);let current=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().is_some_and(|active|active.id==id);if current&&owner.mode==ExtensionMode::Tui&&owner.has_ui{owner.ui.set_widget("btw",Some(panel::widget(question,&reply,false)),Default::default());}});
                tokio::select!{result=collected=>result.map_err(ExtensionFailure::new),()=signal.cancelled()=>Err(ExtensionFailure::new("Side query cancelled"))}
            }.await;
            let current=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().is_some_and(|active|active.id==id);
            if current{match outcome{Ok(reply)=>{if let Some(active)=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_mut(){active.settled=true;}if ctx.mode!=ExtensionMode::Tui||!ctx.has_ui{ctx.ui.notify(&reply,NotificationType::Info);}else{ctx.ui.set_widget("btw",Some(panel::widget(question,&reply,true)),Default::default());}},Err(error)=>{dismiss(&state,ctx,false);ctx.ui.notify(&format!("/btw: {}",error.message),NotificationType::Error);}}}
            Ok(())
        })}));
    }
}

