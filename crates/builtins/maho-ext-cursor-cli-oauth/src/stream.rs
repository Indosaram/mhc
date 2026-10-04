use maho_ai::{types::{AssistantMessage,AssistantMessageEvent as Event,ContentBlock,TextContent,ThinkingContent,Usage,StopReason,DoneReason},model::Model,utils::{event_stream::{AssistantMessageEventStream,create_assistant_message_event_stream},diagnostics::{create_assistant_message_diagnostic,Thrown}}};
use serde_json::{Value,json};
pub struct StreamDeps {
    pub cwd:std::path::PathBuf,pub agent_dir:std::path::PathBuf,pub executable:std::path::PathBuf,
    pub store:std::sync::Arc<dyn maho_ai::auth::types::CredentialStore>,pub oauth:std::sync::Arc<dyn maho_ai::auth::types::OAuthAuth>,
    pub settings:crate::settings::CursorCliOauthProviderSettings,pub router:std::sync::Arc<tokio::sync::Mutex<crate::session_router::SessionRouter>>,
    pub environment:std::collections::BTreeMap<String,String>,pub now:std::sync::Arc<dyn Fn()->i64+Send+Sync>,
}
pub fn stream_cursor_cli(model:Model,context:maho_ai::types::Context,options:Option<maho_ai::types::SimpleStreamOptions>,deps:StreamDeps)->AssistantMessageEventStream {
    let mut mapper=StreamMapper::new(&model,(deps.now)());let stream=mapper.stream.clone();
    tokio::spawn(async move {
        use maho_ai::types::{Message,UserContent,ErrorReason};
        let text=|blocks:&[ContentBlock]|blocks.iter().filter_map(|b|match b {ContentBlock::Text(t) if !t.text.is_empty()=>Some(t.text.as_str()),_=>None}).collect::<Vec<_>>().join("\n");
        let user_text=|content:&UserContent|match content {UserContent::Text(s)=>s.clone(),UserContent::Blocks(blocks)=>text(blocks)};
        let signal=options.as_ref().and_then(|o|o.stream.request.signal.clone());
        let session=options.as_ref().and_then(|o|o.stream.request.affinity_session_id.clone().or_else(||o.stream.session_id.clone())).unwrap_or_else(||crate::affinity::DEFAULT_CURSOR_AFFINITY_KEY.into());
        let prompt=context.messages.iter().rev().find_map(|m|match m {Message::User(u)=>Some(user_text(&u.content)),_=>None});
        let recent:Vec<_>=context.messages.iter().filter_map(|m|match m {
            Message::User(u)=>Some(crate::session_router::RecapExchange {role:crate::session_router::ExchangeRole::User,text:user_text(&u.content)}),
            Message::Assistant(a)=>Some(crate::session_router::RecapExchange {role:crate::session_router::ExchangeRole::Assistant,text:text(&a.content)}),_=>None,
        }).filter(|e|!e.text.is_empty()).collect();let recent=&recent[recent.len().saturating_sub(12)..];
        let spawn_model=crate::spawn_model::resolve_cursor_cli_spawn_model(&model,options.as_ref().and_then(|o|o.thinking_selection.as_ref()));
        let result=async {
            if deps.settings.explicitly_disabled {return Err(json!({"message":"disabled by settings"}));}
            let policy=crate::guardrails::resolve_execution_policy(&deps.settings,&mut Default::default(),&deps.settings.deny_commands).map_err(|e|json!({"message":e.to_string()}))?;
            for warning in &policy.warnings {mapper.apply(&json!({"type":"cursor_chat_restarted","message":warning.message}),&model,0);}
            let prompt=prompt.ok_or_else(||json!({"message":"cursor-cli-oauth needs a user message to prompt the Cursor CLI"}))?;
            let stored=deps.store.read(crate::oauth_login::PROVIDER_ID,None).await.map_err(|e|json!({"message":e.to_string()}))?;
            let slots=stored.as_ref().and_then(maho_ai::auth::types::Credential::as_oauth).map(crate::accounts::list_accounts).transpose().map_err(|e|json!({"message":e.to_string()}))?.unwrap_or_default();
            if !crate::oauth_login::lane_enabled(&deps.settings,slots.len()) {return Err(json!({"message":"disabled by settings"}));}
            if slots.is_empty() {return Err(json!({"message":"no accounts: run /login cursor-cli-oauth"}));}
            let sent_tokens=std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));let emitted_tokens=sent_tokens.clone();
            let outcome=crate::failover::run_failover(crate::failover::FailoverOptions {store:deps.store.as_ref(),provider_id:crate::oauth_login::PROVIDER_ID,affinity:crate::affinity::CursorAffinityOptions {session_id:Some(&session),pinned_account:deps.settings.pinned_account.as_deref(),..Default::default()}},|slot,_| {
                let (sender,receiver)=tokio::sync::mpsc::unbounded_channel();let router=deps.router.clone();let session=session.clone();let prompt=prompt.clone();let recent=recent.iter().map(|e|crate::session_router::RecapExchange {role:match e.role {crate::session_router::ExchangeRole::User=>crate::session_router::ExchangeRole::User,crate::session_router::ExchangeRole::Assistant=>crate::session_router::ExchangeRole::Assistant},text:e.text.clone()}).collect::<Vec<_>>();
                let model=spawn_model.clone();let policy=policy.clone();let cwd=deps.cwd.clone();let agent_dir=deps.agent_dir.clone();let executable=deps.executable.clone();let environment=deps.environment.clone();let signal=signal.clone();let now=deps.now.clone();let tokens=sent_tokens.clone();let store=deps.store.clone();let oauth=deps.oauth.clone();
                let session_policy=crate::session_router::SessionPolicy {resume:deps.settings.resume_mode==crate::settings::ResumeMode::Auto,recap_on_model_switch:deps.settings.context_recap_on_model_switch,..Default::default()};
                tokio::spawn(async move {
                    let mut slot=slot;
                    let refreshed=async {
                        let current=store.read(crate::oauth_login::PROVIDER_ID,None).await?;
                        if let Some(credential)=current.as_ref().and_then(maho_ai::auth::types::Credential::as_oauth) {
                            if let Some(current)=crate::accounts::list_accounts(credential)?.into_iter().find(|s|s.name==slot.name) {slot=current;}
                            if (now() as f64)>=slot.expires {
                                let abort=signal.clone().unwrap_or_else(||maho_ai::utils::abort::AbortController::new().signal());
                                let oauth=oauth.clone();let now=now.clone();
                                store.modify(crate::oauth_login::PROVIDER_ID,Box::new(move |current|Box::pin(async move {
                                    let Some(maho_ai::auth::types::Credential::OAuth(current))=current else {return Ok(current);};
                                    if crate::accounts::list_accounts(&current)?.iter().all(|slot|(now() as f64)<slot.expires) {return Ok(Some(maho_ai::auth::types::Credential::OAuth(current)));}
                                    let refreshed=oauth.refresh(&current,&abort).await?;
                                    Ok(Some(maho_ai::auth::types::Credential::OAuth(refreshed)))
                                })),None).await?;
                                let current=store.read(crate::oauth_login::PROVIDER_ID,None).await?;
                                slot=current.as_ref().and_then(maho_ai::auth::types::Credential::as_oauth).map(crate::accounts::list_accounts).transpose()?.unwrap_or_default().into_iter().find(|s|s.name==slot.name).ok_or_else(||anyhow::anyhow!("cursor-cli-oauth account '{}' disappeared during token refresh",slot.name))?;
                            }
                        }
                        Ok::<(),anyhow::Error>(())
                    }.await;
                    if let Err(error)=refreshed {let _=sender.send(Err(json!({"message":error.to_string()})));return;}
                    let result=crate::session_router::SessionRouter::run_shared_turn(&router,crate::session_router::TurnInput {session:&session,account:&slot.name,prompt:&prompt,model:Some(&model),recent:&recent,policy:&session_policy},|attempt| {
                        tokens.store(maho_core::compaction::compaction::estimate_tokens(&json!({"role":"user","content":attempt.prompt,"timestamp":now()})),std::sync::atomic::Ordering::Relaxed);
                        let receiver=spawn_attempt(SpawnAttemptInput {executable:executable.clone(),cwd:cwd.clone(),agent_dir:agent_dir.clone(),slot:slot.clone(),attempt,model:model.clone(),policy:policy.clone(),environment:environment.clone(),signal:signal.clone()});async move {Ok(receiver)}
                    },||now(),|input|crate::errors::classify_cursor_cli_error(Some(input)).kind,|event|{let _=sender.send(Ok(event));}).await;
                    if let Err(error)=result {let _=sender.send(Err(error));}
                });async move {Ok(receiver)}
            },||(deps.now)() as f64,|event|mapper.apply(&event,&model,emitted_tokens.load(std::sync::atomic::Ordering::Relaxed))).await;
            outcome.map_err(|error|match error {crate::failover::RunError::Attempt(e)=>e.original,crate::failover::RunError::Store(e)=>json!({"message":e.to_string()}),crate::failover::RunError::AllBlocked(e)=>json!({"message":e.to_string()})})
        }.await;
        match result {Ok(())=>mapper.finish(),Err(error)=>{
            mapper.close_open();let aborted=signal.as_ref().is_some_and(maho_ai::utils::abort::AbortSignal::aborted)||error["kind"]=="aborted";
            mapper.output.stop_reason=if aborted {StopReason::Aborted} else {StopReason::Error};mapper.output.error_message=Some(error["message"].as_str().or_else(||error["stderr"].as_str()).unwrap_or("Cursor CLI turn failed").into());
            mapper.stream.push(Event::Error {reason:if aborted {ErrorReason::Aborted} else {ErrorReason::Error},error:mapper.output.clone()});mapper.stream.end(None);
        }}
    });stream
}
pub struct SpawnAttemptInput {
    pub executable:std::path::PathBuf,pub cwd:std::path::PathBuf,pub agent_dir:std::path::PathBuf,pub slot:crate::accounts::CursorCliAccountSlot,
    pub attempt:crate::session_router::SessionAttempt,pub model:String,pub policy:crate::guardrails::ExecutionDecision,
    pub environment:std::collections::BTreeMap<String,String>,pub signal:Option<maho_ai::utils::abort::AbortSignal>,
}
pub fn spawn_attempt(input:SpawnAttemptInput)->crate::session_router::AttemptReceiver {
    let (sender,receiver)=tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
    let SpawnAttemptInput {executable,cwd,agent_dir,slot,attempt,model,policy,environment,signal}=input;
    let producer=sender.clone();
    let result=crate::home_store::run_in_account_home(&agent_dir,&slot,move |home|async move {
        let sender=producer;
        crate::guardrails::apply_deny_config(&home.home,&policy.deny_commands)?;
        let args=crate::spawn_args::CursorCliArgsInput {prompt:&attempt.prompt,model:Some(&model),resume_chat_id:attempt.resume_chat_id.as_deref(),force:policy.force,
            execution_mode:if policy.execution_mode==crate::settings::ExecutionMode::Plan {crate::spawn_args::ExecutionMode::Plan} else {crate::spawn_args::ExecutionMode::Agent},sandbox_mode:policy.sandbox_mode.as_deref()};
        let mut handle=crate::transport::spawn_cursor_cli(&executable,args,home.home.to_str().ok_or_else(||anyhow::anyhow!("account HOME is not UTF-8"))?,&cwd,&environment,signal)?;
        let mut held=Vec::new();let mut saw_result=false;
        loop {
            let event=tokio::select! { event=handle.events.recv()=>event,_=sender.closed()=>{handle.abort();let _=handle.completed.await;return Ok(());} };
            let Some(event)=event else {break;};
            match event {
                Ok(event)=>{
                    if event["type"]=="result" {saw_result=true;}
                    if event["type"]=="aborted" || event["type"]=="malformed_stream" || (event["type"]=="result"&&(event["is_error"]==true||event["subtype"]=="error")) {held.push(event);} else {let _=sender.send(Ok(event));}
                },
                Err(error)=>{let _=sender.send(Err(json!({"thrown":{"message":error.to_string()}})));},
            }
        }
        match handle.completed.await?? {
            crate::transport::TransportOutcome::Aborted=>{let _=sender.send(Err(json!({"kind":"aborted"})));},
            crate::transport::TransportOutcome::Completed {exit_code,stderr,..} if exit_code!=Some(0)&&!saw_result=>{let _=sender.send(Err(json!({"exitCode":exit_code,"stderr":stderr})));},
            crate::transport::TransportOutcome::Completed {..}=>{for event in held {let _=sender.send(Ok(event));}},
        }
        Ok(())
    },|_|{}).await;
    if let Err(error)=result {let _=sender.send(Err(json!({"message":error.to_string()})));}
    });receiver
}
#[derive(Clone,Copy,PartialEq)]
enum OpenKind {Text,Thinking}
pub struct StreamMapper {pub stream:AssistantMessageEventStream,pub output:AssistantMessage,started:bool,open:Option<OpenKind>,index:usize,text:String,accumulated:String}
impl StreamMapper {
    pub fn new(model:&Model,at:i64)->Self {
        Self {stream:create_assistant_message_event_stream(),output:AssistantMessage {content:Vec::new(),api:model.api.clone(),provider:model.provider.clone(),model:model.id.clone(),response_model:None,response_id:None,provider_thinking_level:None,diagnostics:None,usage:Usage::default(),stop_reason:StopReason::Stop,stop_details:None,deferred:None,error_message:None,abort_source:None,raw_stop_reason:None,end_turn:None,timestamp:at},started:false,open:None,index:0,text:String::new(),accumulated:String::new()}
    }
    fn close_open(&mut self) {
        if let Some(kind)=self.open.take() {
            let event=match kind {OpenKind::Text=>Event::TextEnd {content_index:self.index,content:self.text.clone(),partial:self.output.clone()},OpenKind::Thinking=>Event::ThinkingEnd {content_index:self.index,content:self.text.clone(),partial:self.output.clone()}};self.stream.push(event);
        }
    }
    fn ensure_open(&mut self,kind:OpenKind) {
        if self.open==Some(kind) {return;}self.close_open();
        if !self.started {self.started=true;self.stream.push(Event::Start {partial:self.output.clone()});}
        self.output.content.push(match kind {OpenKind::Text=>ContentBlock::Text(TextContent::default()),OpenKind::Thinking=>ContentBlock::Thinking(ThinkingContent::default())});
        self.index=self.output.content.len()-1;self.open=Some(kind);self.text.clear();self.accumulated.clear();
        self.stream.push(match kind {OpenKind::Text=>Event::TextStart {content_index:self.index,partial:self.output.clone()},OpenKind::Thinking=>Event::ThinkingStart {content_index:self.index,partial:self.output.clone()}});
    }
    fn delta(&mut self,text:&str) {
        self.text.push_str(text);
        match &mut self.output.content[self.index] {ContentBlock::Text(block)=>block.text.clone_from(&self.text),ContentBlock::Thinking(block)=>block.thinking.clone_from(&self.text),_=>{}}
        self.stream.push(match self.open {Some(OpenKind::Thinking)=>Event::ThinkingDelta {content_index:self.index,delta:text.into(),partial:self.output.clone()},_=>Event::TextDelta {content_index:self.index,delta:text.into(),partial:self.output.clone()}});
    }
    pub fn apply(&mut self,event:&Value,model:&Model,sent_prompt_tokens:u64) {
        match event["type"].as_str() {
            Some("thinking")=> {
                if event["subtype"]=="completed" {if self.open==Some(OpenKind::Thinking) {self.close_open();}}
                else if let Some(text)=event["text"].as_str().filter(|s|!s.is_empty()) {self.ensure_open(OpenKind::Thinking);self.delta(text);}
            },
            Some("assistant")=> {
                if let Some(blocks)=event["message"]["content"].as_array() {
                    for block in blocks {if let Some(fragment)=block["text"].as_str().filter(|s|!s.is_empty()) {
                        self.ensure_open(OpenKind::Text);
                        let delta=if fragment.len()>=self.accumulated.len()&&fragment.starts_with(&self.accumulated) {let delta=fragment[self.accumulated.len()..].to_owned();self.accumulated=fragment.into();delta}
                            else {self.accumulated.push_str(fragment);fragment.into()};
                        if !delta.is_empty() {self.delta(&delta);}
                    }}
                }
            },
            Some("tool_call")=>{self.close_open();self.accumulated.clear();},
            Some("cursor_account_changed"|"cursor_chat_restarted")=>{if let Some(message)=event["message"].as_str() {self.ensure_open(OpenKind::Text);self.delta(&format!("{message}\n"));}},
            Some("result") if event["subtype"]=="success"&&event["is_error"]!=true=> {
                self.output.usage.input=sent_prompt_tokens;self.output.usage.output=event["usage"]["outputTokens"].as_u64().unwrap_or(0);self.output.usage.cache_read=0;self.output.usage.cache_write=0;self.output.usage.total_tokens=0;
                self.output.usage.cost=maho_ai::models::calculate_cost(model,&mut self.output.usage);
                let details=json!({"inputTokens":event["usage"]["inputTokens"],"outputTokens":event["usage"]["outputTokens"],"cacheReadTokens":event["usage"]["cacheReadTokens"],"cacheWriteTokens":event["usage"]["cacheWriteTokens"],"requestId":event["request_id"],"durationMs":event["duration_ms"]});
                self.output.diagnostics.get_or_insert_with(Vec::new).push(create_assistant_message_diagnostic("cursor_cli_oauth_cli_usage",&Thrown::Value(json!("cursor-agent reported usage (telemetry only; never used for senpi context accounting)")),details.as_object().cloned()));
            },
            _=>{},
        }
    }
    pub fn finish(&mut self) {self.close_open();self.stream.push(Event::Done {reason:DoneReason::Stop,message:self.output.clone()});self.stream.end(None);}
}
#[cfg(test)]
mod tests {
    use super::*;
    fn model()->Model {
        serde_json::from_value(json!({"id":"test","name":"Test","api":"cursor-agent","provider":"cursor-cli-oauth","baseUrl":"cursor-cli-oauth","reasoning":true,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":200000,"maxTokens":64000})).expect("model")
    }
    #[tokio::test]
    async fn snapshots_tool_boundaries_and_telemetry() {
        let model=model();let mut mapper=StreamMapper::new(&model,1);
        for text in ["STREAM","TEST OK","STREAMTEST OK"] {mapper.apply(&json!({"type":"assistant","message":{"content":[{"type":"text","text":text}]}}),&model,7);}
        mapper.apply(&json!({"type":"tool_call","call_id":"secret-tool-frame"}),&model,7);
        mapper.apply(&json!({"type":"thinking","subtype":"delta","text":"thinking"}),&model,7);
        mapper.apply(&json!({"type":"result","subtype":"success","usage":{"inputTokens":900000,"outputTokens":3,"cacheReadTokens":10000,"cacheWriteTokens":5000},"request_id":"r","duration_ms":1,"is_error":false}),&model,7);
        mapper.finish();let events=mapper.stream.collect().await.expect("events");
        assert!(matches!(events.first(),Some(Event::Start {..})));
        assert_eq!(events.iter().filter_map(|e|match e {Event::TextDelta {delta,..}=>Some(delta.as_str()),_=>None}).collect::<Vec<_>>(),["STREAM","TEST OK"]);
        assert_eq!(mapper.output.usage.input,7);assert_eq!(mapper.output.usage.output,3);assert_eq!(mapper.output.usage.cache_read,0);
        assert_eq!(mapper.output.diagnostics.as_ref().expect("telemetry")[0].details.as_ref().expect("details")["inputTokens"],900000);
        assert!(!mapper.output.content.iter().any(|b|matches!(b,ContentBlock::ToolCall(_))));
    }
}
