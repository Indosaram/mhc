use futures::StreamExt;
use serde::{Deserialize,Serialize};
use maho_ai::types::{ContentBlock,StopReason};
use super::super::types::*;
use super::generation::{checkpoint,decode,emit,entry,estimate,head,json,object,retry_decision,user,RetryDecision};

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct CollapseInput {
    pub reason:CompactionReason, pub through:Id,
    #[serde(skip_serializing_if="Option::is_none")] pub instructions:Option<String>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Base {
    pub expected_head:Option<Id>,
    #[serde(skip_serializing_if="Option::is_none")] pub instructions:Option<String>,
    pub model:ModelRef,pub thinking_level:ThinkingLevel,pub retry:RetryPolicy,pub attempt:u32,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(tag="phase",rename_all="lowercase",rename_all_fields="camelCase")]
pub enum CollapseCheckpoint {
    Summarizing {#[serde(flatten)] base:Base},
    Retrying {#[serde(flatten)] base:Base,until_ms:u64,last_error:String},
    Prepared {#[serde(flatten)] base:Base,summary:String},
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct CollapseResult {pub summary:Id}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct CollapseFailureData {pub reason:CompactionFailure,pub detail:String}
#[derive(Clone,Debug)]
pub enum CollapseChoice {Decline,Summary {instructions:Option<String>,summary:Option<String>}}
pub type BeforeCollapse=std::rc::Rc<dyn for<'a> Fn(CompactionReason,Id,Vec<Entry>,HookInfo,&'a Context)->LocalBoxFuture<'a,Result<Option<CollapseChoice>>>>;
#[derive(Default)]
pub struct CollapseHooks {pub before_collapse:Option<BeforeCollapse>}
type C=Task<CollapseInput,CollapseCheckpoint>;
type Rt<'a> = dyn Runtime<CollapseHooks,dyn CoreTx> + 'a;
type S=Step<CollapseCheckpoint,CollapseResult,CollapseFailureData,dyn CoreTx>;
fn failed(reason:CompactionFailure,detail:&str)->Completion<CollapseResult,CollapseFailureData> {
    Completion::Failed {failure:CollapseFailureData {reason,detail:detail.into()}}
}
fn fail(reason:CompactionFailure,detail:String)->S {
    Step::Done(Box::new(move |_,_,_|Box::pin(async move {Ok(failed(reason,&detail))})))
}
pub struct CollapseKind {token:KindToken,config:KindConfig,inflight:Vec<String>}
pub fn collapse()->Result<CollapseKind> {
    Ok(CollapseKind {token:KindToken::core("pi.collapse"),config:KindConfig {rewindable:object(&serde_json::json!({"threshold":0,"keepRecent":20000}))?,sticky:JsonObject::new()},inflight:vec!["summarizing".into()]})
}
impl Kind for CollapseKind {
    type Input=CollapseInput;type Checkpoint=CollapseCheckpoint;type Result=CollapseResult;type Failure=CollapseFailureData;
    type Aborted=();type Hooks=CollapseHooks;type Slot=JsonObject;type Tx=dyn CoreTx;
    fn token(&self)->&KindToken {&self.token}
    fn turn(&self)->bool {false}
    fn config(&self)->&KindConfig {&self.config}
    fn inflight(&self)->&[String] {&self.inflight}
    fn slot(&self,_:&CollapseInput)->Result<JsonObject> {Ok(JsonObject::new())}
    fn describe(&self,task:&C,_:Option<&JsonObject>)->Result<JsonValue> {json(&task.checkpoint)}
    fn initial<'a>(&'a self,task:&'a C,rt:&'a Rt<'a>,ctx:&'a Context)->LocalBoxFuture<'a,Result<S>> {
        Box::pin(async move {
            let state=rt.rewindable(task.conversation_id,ctx).await?;
            let Some(model)=state.model else {return Ok(fail(CompactionFailure::NoModel,"no model configured".into()));};
            let sticky=rt.sticky(task.conversation_id,ctx).await?;
            let expected_head=rt.newest_entry(task.conversation_id,NewestEntryOptions {kind:None,with_head:Some(true)},ctx).await?.map(|e|e.id);
            let entries=rt.context(task.conversation_id,Some(task.input.through),ctx).await?.entries;
            let choice=rt.hooks().first(ctx,|h,api| {let entries=entries.clone();async move {
                match &h.before_collapse {Some(f)=>f(task.input.reason,task.input.through,entries,api,ctx).await,None=>Ok(None)}
            }}).await?;
            let (instructions,summary)=match choice {
                Some(CollapseChoice::Decline)=>return Ok(fail(CompactionFailure::Declined,"declined by beforeCollapse".into())),
                Some(CollapseChoice::Summary {instructions,summary})=>(instructions.or_else(||task.input.instructions.clone()),summary),
                None=>(task.input.instructions.clone(),None),
            };
            let base=Base {expected_head,instructions:instructions.filter(|s|!s.is_empty()),model,thinking_level:state.thinking_level,retry:sticky.retry,attempt:1};
            if let Some(summary)=summary {
                return Ok(Step::Build(Box::new(move |tx,current,_|Box::pin(async move {
                    if head(tx,current.conversation_id).await?!=base.expected_head {return Ok(Transition::Done(failed(CompactionFailure::Stale,"head moved during beforeCollapse")));}
                    Ok(Transition::Checkpoint(CollapseCheckpoint::Prepared {base,summary}))
                }))));
            }
            summarize(task,base,rt,ctx,true).await
        })
    }
    fn phase<'a>(&'a self,phase:&'a str,task:&'a C,rt:&'a Rt<'a>,ctx:&'a Context)->LocalBoxFuture<'a,Result<S>> {
        Box::pin(async move {
            let cp=task.checkpoint.as_ref().ok_or_else(||Error::InvalidShape("collapse phase without checkpoint".into()))?;
            match (phase,cp) {
                ("summarizing",CollapseCheckpoint::Summarizing {base})=>Ok(after_failure(base.clone(),None,"interrupted".into(),rt.now())),
                ("retrying",CollapseCheckpoint::Retrying {base,until_ms,..})=> {
                    rt.sleep(*until_ms,ctx).await?;let mut base=base.clone();base.attempt=base.attempt.saturating_add(1);
                    summarize(task,base,rt,ctx,false).await
                },
                ("prepared",CollapseCheckpoint::Prepared {base,summary})=> {
                    Ok(prepared(base.clone(),summary.clone(),task.input.through,rt.now()))
                },
                _=>Err(Error::InvalidShape(format!("collapse phase/checkpoint mismatch: {phase}"))),
            }
        })
    }
    fn abort<'a>(&'a self,_:&'a C,_:&'a Rt<'a>,_:&'a Context)->LocalBoxFuture<'a,Result<AbortClosure<(),dyn CoreTx>>> {
        Box::pin(async {
            let closure: AbortClosure<(),dyn CoreTx> = Box::new(|_,_,_|Box::pin(async {Ok(())}));
            Ok(closure)
        })
    }
}
fn prepared(base:Base,summary:String,through:Id,now:u64)->S {
    Step::Done(Box::new(move |tx,current,_|Box::pin(async move {
        let context=tx.context(current.conversation_id,None).await?;
        if context.head.map(|e|e.id)!=base.expected_head {return Ok(failed(CompactionFailure::Stale,"head moved during collapse"));}
        let retained=context.entries.iter().find(|e|e.id>through).map(|e|e.id);
        let mut e=entry("pi.summary");e.data=Some(object(&serde_json::json!({"through":through}))?);
        e.model=Some(vec![user(&summary,now)?]);e.head=Some(retained.map_or(EntryHead::Own(SelfHead::Own),EntryHead::Id));
        let summary=tx.append_entry(current.conversation_id,e)?;
        Ok(Completion::Completed {result:CollapseResult {summary}})
    })))
}
async fn summarize(task:&C,base:Base,rt:&Rt<'_>,ctx:&Context,check_head:bool)->Result<S> {
    let saved=base.clone();
    let stale=rt.commit(Box::new(move |tx,current,_|Box::pin(async move {
        if check_head&&head(tx,current.conversation_id).await?!=saved.expected_head {return json(&true);}
        checkpoint(tx,&CollapseCheckpoint::Summarizing {base:saved})?;json(&false)
    })),ctx).await?;
    if decode::<bool>(&stale)? {return Ok(fail(CompactionFailure::Stale,"head moved during beforeCollapse".into()));}
    let Some(model)=rt.models().resolve(&base.model) else {return Ok(fail(CompactionFailure::NoModel,"model unavailable".into()));};
    let mut messages=rt.context(task.conversation_id,Some(task.input.through),ctx).await?.messages;
    let tools=super::super::system::effective_tools(&messages)?;
    if !tools.is_empty() {messages.push(StoredMessage::from_json(json(&serde_json::json!({"role":"system","content":"","toolsRemoved":tools.iter().map(|t|serde_json::json!({"name":t.name})).collect::<Vec<_>>(),"timestamp":rt.now()}))?));}
    messages.push(user(&format!("{}\n\nRespond with the summary only.",base.instructions.as_deref().unwrap_or("Summarize the conversation so far for continuation.")),rt.now())?);
    let mut stream=rt.models().stream(&model,RequestOptions {messages,thinking_level:base.thinking_level.clone()},ctx);
    let mut terminal=None;
    while let Some(event)=stream.next().await {
        match event {
            AssistantMessageEvent::Done {message,..}=> {terminal=Some(message);break;},
            AssistantMessageEvent::Error {error,..}=> {terminal=Some(error);break;},
            AssistantMessageEvent::Start {..}|AssistantMessageEvent::TextStart {..}|AssistantMessageEvent::TextDelta {..}|AssistantMessageEvent::TextEnd {..}|AssistantMessageEvent::ThinkingStart {..}|AssistantMessageEvent::ThinkingDelta {..}|AssistantMessageEvent::ThinkingEnd {..}|AssistantMessageEvent::ToolcallStart {..}|AssistantMessageEvent::ToolcallDelta {..}|AssistantMessageEvent::ToolcallEnd {..}=>{}
        }
    }
    let Some(mut message)=terminal else {return Ok(after_failure(base,None,"no message".into(),rt.now()));};
    if message.content.iter().any(|b|matches!(b,ContentBlock::ToolCall(_))) {
        message.stop_reason=StopReason::Error;
        return Ok(after_failure(base,Some(&message),"summarizer returned tool calls".into(),rt.now()));
    }
    if message.stop_reason==StopReason::Error {
        return Ok(after_failure(base,Some(&message),message.error_message.clone().unwrap_or_else(||"provider error".into()),rt.now()));
    }
    let summary=message.content.iter().filter_map(|b|match b {ContentBlock::Text(t)=>Some(t.text.as_str()),_=>None}).collect::<String>();
    Ok(Step::Next(CollapseCheckpoint::Prepared {base,summary}))
}
fn after_failure(base:Base,message:Option<&AssistantMessage>,detail:String,now:u64)->S {
    match retry_decision(&base.retry,base.attempt,message,now) {
        RetryDecision::Fail {exhausted}=>fail(if exhausted {CompactionFailure::RetriesExhausted}else{CompactionFailure::Provider},detail),
        RetryDecision::Retry {until_ms}=>Step::Build(Box::new(move |tx,current,_|Box::pin(async move {
            emit(tx,CoreViewEvent::CompactionRetrying {task_id:current.id,attempt:base.attempt,retry_at:until_ms,error:detail.clone()})?;
            Ok(Transition::Checkpoint(CollapseCheckpoint::Retrying {base,until_ms,last_error:detail}))
        }))),
    }
}
pub fn choose_through(entries:&[Entry],keep_recent:u64)->Result<Option<Id>> {
    let mut exchanges:Vec<(Id,u64)>=Vec::new();let mut open=false;
    for e in entries {
        let messages=e.model.as_deref().unwrap_or(&[]);let tokens=estimate(messages,false)?;
        let role=messages.first().map(StoredMessage::decode).transpose()?;
        match role {
            Some(Message::Assistant(_))=> {exchanges.push((e.id,tokens));open=true;},
            Some(Message::ToolResult(_)) if open=> {
                if let Some(last)=exchanges.last_mut() {last.0=e.id;last.1=last.1.saturating_add(tokens);}
            },
            Some(Message::ToolResult(_))|Some(Message::System(_))|Some(Message::User(_))|None=> {open=false;exchanges.push((e.id,tokens));},
        }
    }
    let mut retained=0u64;let mut count=exchanges.len();
    while let Some((_,tokens))=count.checked_sub(1).and_then(|i|exchanges.get(i)) {
        if retained.saturating_add(*tokens)>keep_recent {break;}
        retained=retained.saturating_add(*tokens);count=count.saturating_sub(1);
    }
    if count==exchanges.len() {count=count.saturating_sub(1);}
    Ok(count.checked_sub(1).and_then(|i|exchanges.get(i)).map(|e|e.0))
}

#[cfg(test)]
#[path="collapse_tests.rs"]
mod tests;
