#[path="native_account/support.rs"]
pub mod support;
use maho_ext_api::*;
use maho_ai::types::{Context,SimpleStreamOptions,AssistantMessageEvent,DoneReason};
use maho_ai::utils::event_stream::AssistantMessageEventStream;
use std::sync::{Arc,Mutex};
struct Registry{calls:Mutex<Vec<(Context,SimpleStreamOptions)>>,entered:tokio::sync::watch::Sender<Option<maho_ai::utils::abort::AbortSignal>>,blocked:bool}
impl ModelRegistry for Registry{
    fn get_all(&self)->Vec<Model>{vec![]}
    fn get_available(&self)->Vec<Model>{vec![]}
    fn find(&self,_:&str,_:&str)->Option<Model>{None}
    fn has_configured_auth(&self,_:&Model)->bool{true}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{panic!("resolved auth required")})}
    fn get_api_key_and_headers<'a>(&'a self,_:&'a Model)->ExtensionFuture<'a,ResolvedRequestAuth>{Box::pin(async{Ok(ResolvedRequestAuth{auth:maho_ai::models::ProviderAuthResult{api_key:Some("side-secret".into()),..Default::default()},extra_body:Some(serde_json::Map::from_iter([("fixture".into(),serde_json::json!(true))])),upstream_model_id:None,service_tier:None,env:None})})}
    fn stream_simple(&self,_:&Model,context:&Context,options:Option<SimpleStreamOptions>)->Result<AssistantMessageEventStream,ExtensionFailure>{
        let options=options.expect("options");self.entered.send_replace(options.stream.request.signal.clone());self.calls.lock().expect("calls").push((context.clone(),options));
        let stream=AssistantMessageEventStream::assistant();
        if !self.blocked{
            let message:AssistantMessage=serde_json::from_value(serde_json::json!({"role":"assistant","api":"faux","provider":"fixture","model":"m","timestamp":0,"content":[],"stopReason":"stop","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}}})).expect("message");
            stream.push(AssistantMessageEvent::TextDelta{content_index:0,delta:"side reply".into(),partial:message.clone()});stream.push(AssistantMessageEvent::Done{reason:DoneReason::Stop,message});
        }
        Ok(stream)
    }
}
#[tokio::test]
async fn registered_side_query_uses_real_stream_auth_off_snapshot_and_bare_cancel()->Result<(),Box<dyn std::error::Error>>{
    for blocked in [false,true]{
        let (entered,mut observed)=tokio::sync::watch::channel(None);let registry=Arc::new(Registry{calls:Mutex::new(vec![]),entered,blocked});let ui=Arc::new(support::TestUi::default());
        let mut ctx=support::context(registry.clone(),ui.clone());ctx.model=Some(serde_json::from_value(serde_json::json!({"id":"m","name":"M","api":"openai-completions","provider":"fixture","baseUrl":"https://example.invalid","reasoning":true,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":10000,"maxTokens":1000}))?);
        let mut api=ExtensionApi::new(LoadedExtension::new("btw","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
        maho_ext_btw::Btw{thinking_level:Arc::new(|_|Ok(None))}.register(&mut api);
        let handler=api.registered.commands.iter().find(|command|command.name=="btw").expect("command").handler.clone();
        if blocked{
            let command_ctx=ctx.clone();let pending=handler.clone();
            let mut work=tokio::spawn(async move{pending("question",&command_ctx).await});
            let started=tokio::time::timeout(std::time::Duration::from_secs(5),observed.changed()).await;
            if !matches!(started,Ok(Ok(()))){work.abort();let _result=work.await;return Err("stream did not start".into());}
            let signal=observed.borrow().clone().ok_or("provider signal")?;
            let cancel=handler("",&ctx).await;
            let completed=tokio::time::timeout(std::time::Duration::from_secs(5),&mut work).await;
            if completed.is_err(){work.abort();let _result=work.await;}
            cancel?;completed???;assert!(signal.aborted());assert!(ui.0.lock().expect("notifications").is_empty());
        }else{
            handler("question",&ctx).await?;
            assert_eq!(ui.0.lock().expect("notifications").last().expect("reply").0,"side reply");
            handler("",&ctx).await?;
        }
        let calls=registry.calls.lock().expect("calls");assert_eq!(calls.len(),1);assert_eq!(calls[0].0.tools,Some(vec![]));assert_eq!(calls[0].0.messages.len(),1);
        assert_eq!(calls[0].1.reasoning,None);assert_eq!(calls[0].1.stream.extra_body.as_ref().expect("body")["fixture"],true);
        assert_eq!(calls[0].1.stream.request.api_key.as_deref(),Some("side-secret"));assert_eq!(calls[0].1.stream.request.affinity_session_id.as_deref(),Some("session:btw:1"));
        assert!(!ui.0.lock().expect("notifications").iter().any(|(message,_)|message.contains("side-secret")));assert!(ctx.session_manager.get_entries().is_empty());
    }
    Ok(())
}
