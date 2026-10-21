pub mod side_query;
pub mod panel;

use maho_ext_api::*;
use std::sync::{Arc,Mutex};
struct Active {id:u64,controller:maho_ai::utils::abort::AbortController,settled:bool,unsubscribe:Option<UiUnsubscribe>,panel:Option<Arc<Mutex<panel::PanelState>>>,completion:tokio::sync::watch::Receiver<bool>}
struct State {next:u64,active:Option<Active>,retiring:Vec<tokio::sync::watch::Receiver<bool>>}
struct Completion(tokio::sync::watch::Sender<bool>);
impl Drop for Completion {fn drop(&mut self){self.0.send_replace(true);}}
fn dismiss(state:&Mutex<State>,ctx:&ExtensionContext,abort:bool)->Vec<tokio::sync::watch::Receiver<bool>>{
    let (active,retiring)={let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.next+=1;let active=state.active.take();state.retiring.retain(|completion| !*completion.borrow());if let Some(active)=&active{state.retiring.push(active.completion.clone());}(active,state.retiring.clone())};
    if let Some(active)=active{if abort{active.controller.abort(None);}
        if let Some(unsubscribe)=active.unsubscribe{unsubscribe();}ctx.ui.set_widget("btw",None,Default::default());}
    retiring
}
#[expect(clippy::expect_used, reason = "the command completion guard publishes before dropping its sender")]
async fn await_retirement(retiring:Vec<tokio::sync::watch::Receiver<bool>>){
    for mut completion in retiring {completion.wait_for(|done|*done).await.expect("command completion guard owns sender");}
}
async fn collect_with_signal(
    stream:&maho_ai::utils::event_stream::AssistantMessageEventStream,
    signal:&maho_ai::utils::abort::AbortSignal,
    on_text_delta:impl FnMut(&str),
)->Result<String,ExtensionFailure>{
    let collected=side_query::collect_reply(stream,side_query::DEFAULT_ESTABLISHMENT_TIMEOUT_MS,on_text_delta);
    tokio::pin!(collected);
    tokio::select!{
        biased;
        ()=signal.cancelled()=>{
            let cleanup=stream.cancel().await;
            let outcome=collected.await;
            cleanup.map_err(|error|ExtensionFailure::new(error.message))?;
            outcome.map_err(ExtensionFailure::new)?;
            Err(ExtensionFailure::new("Side query cancelled"))
        },
        result=&mut collected=>result.map_err(ExtensionFailure::new),
    }
}
pub type ThinkingLevelReader=Arc<dyn Fn(&ExtensionContext)->Result<Option<maho_ai::types::ThinkingLevel>,ExtensionFailure>+Send+Sync>;
pub struct Btw{pub thinking_level:ThinkingLevelReader}
impl Default for Btw {
    fn default() -> Self { Self { thinking_level: Arc::new(|ctx: &ExtensionContext| ctx.current_thinking_level()) } }
}
impl Extension for Btw{
    fn register(&self,api:&mut ExtensionApi){
        let state=Arc::new(Mutex::new(State{next:0,active:None,retiring:Vec::new()}));
        for kind in [EventKind::SessionBeforeSwitch,EventKind::SessionBeforeFork,EventKind::SessionShutdown,EventKind::Input]{
            let state=state.clone();api.on(kind,Arc::new(move|_,ctx|{let state=state.clone();Box::pin(async move{let close=kind!=EventKind::Input||state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().is_some_and(|active|active.settled);if close{let retiring=dismiss(&state,ctx,kind!=EventKind::Input);await_retirement(retiring).await;}Ok(EventResult::None)})}));
        }
        let thinking_level=self.thinking_level.clone();
        api.register_command("btw",Some("Ask a side question in parallel without touching the main session".into()),Some("<question>".into()),Arc::new(move|args,ctx|{
            let question=args.trim();
            if question.is_empty(){let retiring=if state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.is_some(){dismiss(&state,ctx,true)}else{ctx.ui.notify("Usage: /btw <question>",NotificationType::Warning);state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retiring.clone()};return Box::pin(async move {await_retirement(retiring).await;Ok(())});}
            let Some(model)=ctx.model.clone()else{ctx.ui.notify("No active model available for /btw.",NotificationType::Error);return Box::pin(async {Ok(())});};
            let entries=ctx.session_manager.get_entries().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
            let snapshot=maho_core::session_manager::build_session_context(&entries,ctx.session_manager.get_leaf_id().as_deref());
            let history=match maho_core::messages::convert_to_llm(&maho_core::messages::filter_context_excluded_messages(snapshot.messages)).into_iter().map(|message|serde_json::from_value(message).map_err(|error|ExtensionFailure::new(error.to_string()))).collect::<Result<Vec<_>,_>>(){Ok(history)=>history,Err(error)=>return Box::pin(async move{Err(error)})};
            let system_prompt=ctx.get_system_prompt();
            let thinking_level=thinking_level.clone();
            let state=state.clone();
            Box::pin(async move{
            let thinking_level=thinking_level(ctx)?;
            let context=side_query::build_side_query_context(&system_prompt,history,question,&model).map_err(ExtensionFailure::new)?;
            let retiring=dismiss(&state,ctx,true);
            let id=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).next;
            await_retirement(retiring).await;
            let controller=maho_ai::utils::abort::AbortController::new();let signal=controller.signal();
            let (completed,completion)=tokio::sync::watch::channel(false);
            let _completion=Completion(completed);
            {let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if state.next!=id{return Ok(());}state.active=Some(Active{id,controller,settled:false,unsubscribe:None,panel:None,completion});}
            if ctx.mode==ExtensionMode::Tui&&ctx.has_ui{
                let panel_state=Arc::new(Mutex::new(panel::PanelState{answer:String::new(),status:panel::BtwPanelStatus::Streaming}));
                ctx.ui.set_widget("btw",Some(panel::widget(question,panel_state.clone())),Default::default());
                if let Some(active)=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_mut(){active.panel=Some(panel_state.clone());}
                let callback=state.clone();let owner=ctx.clone();
                let subscription=ctx.ui.on_terminal_input(Arc::new(move|data|{if !maho_tui::keys::is_key_release(data)&&maho_tui::keys::matches_key(data,"escape")&&callback.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().is_some_and(|active|active.id==id){drop(dismiss(&callback,&owner,true));}None}));
                let unsubscribe=match subscription{Ok(unsubscribe)=>unsubscribe,Err(error)=>{drop(dismiss(&state,ctx,true));return Err(error);}};
                let mut unsubscribe=Some(unsubscribe);
                {let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);if let Some(active)=state.active.as_mut().filter(|active|active.id==id){active.unsubscribe=unsubscribe.take();}}
                if let Some(unsubscribe)=unsubscribe{unsubscribe();}
            }
            let outcome=async{
                let resolved=tokio::select!{biased;()=signal.cancelled()=>return Err(ExtensionFailure::new("Side query cancelled")),auth=ctx.model_registry.get_api_key_and_headers(&model)=>auth};
                let auth=match resolved{
                    Ok(auth)=>auth,
                    Err(error)=>{
                        let current=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().is_some_and(|active|active.id==id);
                        if current{drop(dismiss(&state,ctx,true));ctx.ui.notify(&format!("/btw: {}",error.message),NotificationType::Error);}
                        return Err(error);
                    }
                };
                if signal.aborted(){return Err(ExtensionFailure::new("Side query cancelled"));}
                let mut options=maho_ai::types::SimpleStreamOptions::default();options.stream.request.api_key=auth.auth.api_key;options.stream.request.headers=auth.auth.headers;options.stream.request.env=auth.env;options.stream.request.signal=Some(signal.clone());options.stream.request.affinity_session_id=Some(format!("{}:btw:{id}",ctx.session_manager.session_id()));options.stream.extra_body=auth.extra_body;options.reasoning=thinking_level;
                let stream=ctx.model_registry.stream_simple(&model,&context,Some(options))?;
                let mut reply=String::new();
                collect_with_signal(&stream,&signal,|delta|{reply.push_str(delta);if let Some(panel)=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().filter(|active|active.id==id).and_then(|active|active.panel.clone()){panel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).answer.push_str(delta);}}).await
            }.await;
            if outcome.is_err(){
                let controller=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().filter(|active|active.id==id).map(|active|active.controller.clone());
                if let Some(controller)=controller{controller.abort(None);}
            }
            let current=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().is_some_and(|active|active.id==id);
            if current{match outcome{Ok(reply)=>{if let Some(active)=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_mut(){active.settled=true;}
                if ctx.mode!=ExtensionMode::Tui||!ctx.has_ui{ctx.ui.notify(&reply,NotificationType::Info);}else if let Some(panel)=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().and_then(|active|active.panel.clone()){panel.lock().unwrap_or_else(std::sync::PoisonError::into_inner).status=panel::BtwPanelStatus::Done;}},Err(error)=>{
                if let Some(active)=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_mut(){active.settled=true;}
                if ctx.mode==ExtensionMode::Tui&&ctx.has_ui{if let Some(panel)=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).active.as_ref().and_then(|active|active.panel.clone()){let mut panel=panel.lock().unwrap_or_else(std::sync::PoisonError::into_inner);panel.status=panel::BtwPanelStatus::Error(error.message.clone());}}else{ctx.ui.notify(&format!("/btw failed: {}",error.message),NotificationType::Error);}
            }}}
            Ok(())
        })}));
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_collection_waits_for_handler_and_relay_cleanup() {
        // Given an active stream with separately gated handler and relay cleanup.
        let stream=maho_ai::utils::event_stream::AssistantMessageEventStream::assistant();
        let controller=maho_ai::utils::abort::AbortController::new();
        let signal=controller.signal();
        let (entered_tx,entered_rx)=tokio::sync::oneshot::channel();
        let (release_tx,release_rx)=tokio::sync::oneshot::channel();
        let (relay_entered_tx,relay_entered_rx)=tokio::sync::oneshot::channel();
        let (relay_tx,relay_rx)=tokio::sync::oneshot::channel();
        stream.set_cancellation_handler(Box::new(move||Box::pin(async move{
            entered_tx.send(()).expect("cleanup subscriber");
            release_rx.await.expect("cleanup release");
            relay_entered_tx.send(()).expect("relay subscriber");
            Ok(())
        })));
        stream.set_cancellation_task(tokio::spawn(async move{relay_rx.await.expect("relay release");}));
        let task=tokio::spawn(async move{collect_with_signal(&stream,&signal,|_|{}).await});
        // When cancellation wins the outer collector select.
        controller.abort(None);
        let bound=std::time::Duration::from_secs(5);
        tokio::time::timeout(bound,entered_rx).await.expect("cleanup deadline").expect("cleanup started");
        // Then returning is blocked by both cleanup acknowledgements.
        assert!(!task.is_finished());
        release_tx.send(()).expect("release subscriber");
        tokio::time::timeout(bound,relay_entered_rx).await.expect("relay deadline").expect("handler finished");
        assert!(!task.is_finished());
        relay_tx.send(()).expect("relay release subscriber");
        assert!(tokio::time::timeout(bound,task).await.expect("collector deadline").expect("task").is_err());
    }

    #[tokio::test]
    async fn cancelled_collection_preserves_cleanup_failure() {
        // Given a stream whose cancellation handler fails with a transport error.
        let stream=maho_ai::utils::event_stream::AssistantMessageEventStream::assistant();
        stream.set_cancellation_handler(Box::new(||Box::pin(async{
            Err(maho_ai::utils::event_stream::StreamError::new("cleanup transport failure"))
        })));
        let controller=maho_ai::utils::abort::AbortController::new();
        // When the caller cancels collection.
        controller.abort(None);
        let result=tokio::time::timeout(std::time::Duration::from_secs(5),collect_with_signal(&stream,&controller.signal(),|_|{})).await.expect("collector deadline");
        // Then the cleanup failure, not the generic cancellation, reaches the caller.
        assert_eq!(result.expect_err("cleanup failure").message,"cleanup transport failure");
    }

    #[tokio::test]
    async fn replacement_waits_for_retired_command_completion() {
        // Given the same completion guard owned by an active command.
        let (completed,completion)=tokio::sync::watch::channel(false);
        let guard=Completion(completed);
        let (entered_tx,entered_rx)=tokio::sync::oneshot::channel();
        let replacement=tokio::spawn(async move{
            let retirement=await_retirement(vec![completion]);
            tokio::pin!(retirement);
            std::future::poll_fn(|cx|{
                assert!(std::future::Future::poll(retirement.as_mut(),cx).is_pending());
                std::task::Poll::Ready(())
            }).await;
            entered_tx.send(()).expect("retirement subscriber");
            // When a replacement awaits the previous command's cleanup.
            retirement.await;
            2
        });
        let bound=std::time::Duration::from_secs(5);
        tokio::time::timeout(bound,entered_rx).await.expect("retirement deadline").expect("retirement started");
        // Then no replacement proceeds until that command has released its resources.
        assert!(!replacement.is_finished());
        drop(guard);
        assert_eq!(tokio::time::timeout(bound,replacement).await.expect("replacement deadline").expect("task"),2);
    }
}

