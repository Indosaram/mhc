use maho_ai::{types::{AssistantMessage,AssistantMessageEvent as Event,ContentBlock,TextContent,ThinkingContent,Usage,StopReason,DoneReason},model::Model,utils::{event_stream::{AssistantMessageEventStream,create_assistant_message_event_stream},diagnostics::{create_assistant_message_diagnostic,Thrown}}};
use serde_json::{Value,json};
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
                    if event["type"]=="aborted" || event["type"]=="malformed_stream" {held.push(event);} else {let _=sender.send(Ok(event));}
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
