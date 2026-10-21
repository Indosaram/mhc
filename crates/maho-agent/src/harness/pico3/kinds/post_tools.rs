use serde::{Deserialize,Serialize};
use maho_ai::types::ContentBlock;
use super::super::types::*;
use super::generation::{clear_turn,create,decode,emit,entry,head,json,object,user,GenerationInput};

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct PostToolsInput {pub inputs:Vec<Id>,pub assistant:Id,pub tools:Vec<Id>}
#[derive(Clone,Debug,Default,Serialize,Deserialize)]
pub struct PostToolsResult {
    #[serde(skip_serializing_if="Option::is_none")] pub successor:Option<Id>,
    #[serde(skip_serializing_if="Option::is_none")] pub ended:Option<Ended>,
}
#[derive(Clone,Debug,Serialize,Deserialize,PartialEq,Eq)]
#[serde(rename_all="lowercase")]
pub enum Ended {Terminate,Handoff}
pub type AfterTools=std::rc::Rc<dyn for<'a> Fn(Id,Vec<Id>,HookInfo,&'a Context)->LocalBoxFuture<'a,Result<()>>>;
#[derive(Default)]
pub struct PostToolsHooks {pub after_tools:Option<AfterTools>}
#[derive(Clone,Debug,Serialize,Deserialize)]
struct Row {call:ToolCall,entry:Option<Id>,control:Option<ToolControl>,missing:Option<Missing>}
#[derive(Clone,Copy,Debug,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
enum Missing {Orphaned,Aborted}
impl Missing {fn name(self)->&'static str {match self {Self::Orphaned=>"orphaned",Self::Aborted=>"aborted"}}}
#[derive(Deserialize)]
struct ToolOutcome {entry:Option<Id>,control:Option<ToolControl>}
type P=Task<PostToolsInput,Checkpoint>;
type Rt<'a> = dyn Runtime<PostToolsHooks,dyn CoreTx> + 'a;
type S=Step<Checkpoint,PostToolsResult,JsonValue,dyn CoreTx>;
pub struct PostToolsKind {token:KindToken,config:KindConfig}
pub fn post_tools()->Result<PostToolsKind> {
    Ok(PostToolsKind {token:KindToken::core("pi.post_tools"),config:KindConfig {rewindable:JsonObject::new(),sticky:object(&serde_json::json!({"steeringMode":"one-at-a-time","followUpMode":"one-at-a-time"}))?}})
}
fn row(call:ToolCall,task:&Task)->Result<Row> {
    if task.status!=TaskStatus::Terminal {return Err(Error::InvalidShape("post_tools dependency is not terminal".into()));}
    let (entry,control,missing)=match &task.outcome {
        Some(Outcome::Completed {result})=> {let result:ToolOutcome=decode(result)?;(result.entry,result.control,None)},
        Some(Outcome::Aborted {result})=> {let result:ToolOutcome=decode(result)?;let missing=if result.entry.is_none() {Some(Missing::Aborted)}else{None};(result.entry,None,missing)},
        Some(Outcome::Failed {..})|Some(Outcome::Orphaned)|Some(Outcome::Faulted {..})|None=>(None,None,Some(Missing::Orphaned)),
    };
    Ok(Row {call,entry,control,missing})
}
fn controls(rows:&[Row],selected:&mut Vec<String>)->(bool,Option<String>) {
    let mut terminate=false;let mut handoff=None;
    for control in rows.iter().filter_map(|r|r.control.as_ref()) {
        for name in control.add_tools.iter().flatten() {if !selected.contains(name) {selected.push(name.clone());}}
        terminate|=control.terminate.is_some();
        if let Some(text)=&control.handoff {handoff=Some(text.clone());}
    }
    (terminate,handoff)
}
impl Kind for PostToolsKind {
    type Input=PostToolsInput;type Checkpoint=Checkpoint;type Result=PostToolsResult;type Failure=JsonValue;
    type Aborted=();type Hooks=PostToolsHooks;type Slot=JsonObject;type Tx=dyn CoreTx;
    fn token(&self)->&KindToken {&self.token}
    fn turn(&self)->bool {true}
    fn config(&self)->&KindConfig {&self.config}
    fn inflight(&self)->&[String] {&[]}
    fn slot(&self,_:&PostToolsInput)->Result<JsonObject> {Ok(JsonObject::new())}
    fn describe(&self,_:&P,_:Option<&JsonObject>)->Result<JsonValue> {Ok(JsonValue::Null)}
    fn initial<'a>(&'a self,task:&'a P,rt:&'a Rt<'a>,ctx:&'a Context)->LocalBoxFuture<'a,Result<S>> {
        Box::pin(async move {
            let input=task.input.clone();
            let rows=rt.commit(Box::new(move |tx,_,_|Box::pin(async move {
                let assistant=tx.entry(input.assistant).await?.ok_or_else(||Error::InvalidShape("post_tools assistant missing".into()))?;
                let message=assistant.model.as_ref().and_then(|m|m.first()).ok_or_else(||Error::InvalidShape("post_tools assistant has no model message".into()))?.decode()?;
                let calls=match message {Message::Assistant(m)=>m.content.into_iter().filter_map(|b|match b {ContentBlock::ToolCall(call)=>Some(call),_=>None}).collect::<Vec<_>>(),Message::System(_)|Message::User(_)|Message::ToolResult(_)=>return Err(Error::InvalidShape("post_tools entry is not assistant".into()))};
                if calls.len()!=input.tools.len() {return Err(Error::InvalidShape("post_tools dependency/call count mismatch".into()));}
                let mut rows=Vec::with_capacity(calls.len());
                for (call,id) in calls.into_iter().zip(input.tools) {
                    let task=tx.task(id).await?.ok_or_else(||Error::InvalidShape(format!("post_tools task {id} missing")))?;
                    rows.push(row(call,&task)?);
                }
                json(&rows)
            })),ctx).await?;
            let rows:Vec<Row>=decode(&rows)?;let results:Vec<_>=rows.iter().filter_map(|r|r.entry).collect();
            rt.hooks().each::<(),_,_>(ctx,|h,api| {let results=results.clone();async move {
                if let Some(f)=&h.after_tools {f(task.input.assistant,results,api,ctx).await?;}Ok(None)
            }},|()|false).await?;
            let input=task.input.clone();let now=rt.now();
            Ok(Step::Done(Box::new(move |tx,current,_|Box::pin(async move {
                let c=current.conversation_id;let mut boundary=head(tx,c).await?;
                let state:RewindableState=decode(&tx.rewindable(c)?.value()?)?;let mut selected=state.selected_tools;
                let before=selected.len();let (terminate,handoff)=controls(&rows,&mut selected);
                if selected.len()!=before {tx.rewindable(c)?.set("selectedTools",&json(&selected)?)?;emit(tx,CoreViewEvent::ConfigChanged {keys:vec!["selectedTools".into()]})?;}
                for row in &rows {
                    if let Some(missing)=row.missing {
                        let code=missing.name();let mut e=entry("pi.tool_result");
                        e.model=Some(vec![StoredMessage::from_json(json(&serde_json::json!({"role":"toolResult","toolCallId":row.call.id,"toolName":row.call.name,"content":[{"type":"text","text":format!("Tool result unavailable: task {code}.")}],"isError":true,"timestamp":now}))?)]);
                        e.data=Some(object(&serde_json::json!({"diagnostics":[{"severity":"error","message":code,"code":code}]}))?);
                        tx.append_entry(c,e)?;emit(tx,CoreViewEvent::Warning {source:"post_tools".into(),message:format!("synthesized {code} tool result for {}",row.call.id)})?;
                    }
                }
                if let Some(text)=&handoff {let mut e=entry("pi.handoff");e.head=Some(EntryHead::Own(SelfHead::Own));e.model=Some(vec![user(text,now)?]);boundary=Some(tx.append_entry(c,e)?);}
                if handoff.is_some()||terminate {
                    clear_turn(tx,c)?;tx.resolve_inputs(&input.inputs,InputResolution::Done {answer:input.assistant}).await?;
                    emit(tx,CoreViewEvent::TurnEnded {inputs:input.inputs,resolution:TurnResolution::Done {answer:input.assistant}})?;
                    let placed=tx.boundary(c,Boundary::Final,boundary).await?;
                    let successor=successor(tx,placed.triggers)?;
                    return Ok(Completion::Completed {result:PostToolsResult {successor,ended:Some(if handoff.is_some() {Ended::Handoff}else{Ended::Terminate})}});
                }
                let placed=tx.boundary(c,Boundary::PostTools,boundary).await?;
                if placed.terminated {
                    clear_turn(tx,c)?;tx.resolve_inputs(&input.inputs,InputResolution::Unanswered {reason:InputReason::Terminated,detail:None}).await?;
                    emit(tx,CoreViewEvent::TurnEnded {inputs:input.inputs,resolution:TurnResolution::Unanswered {reason:InputReason::Terminated,detail:None}})?;
                    return Ok(Completion::Completed {result:PostToolsResult {successor:successor(tx,placed.triggers)?,ended:None}});
                }
                let mut inputs=input.inputs;inputs.extend(placed.triggers);
                Ok(Completion::Completed {result:PostToolsResult {successor:Some(create(tx,"pi.generation",&GenerationInput {inputs},vec![])?),ended:None}})
            }))))
        })
    }
    fn phase<'a>(&'a self,phase:&'a str,_:&'a P,_:&'a Rt<'a>,_:&'a Context)->LocalBoxFuture<'a,Result<S>> {
        Box::pin(async move {Err(Error::InvalidShape(format!("post_tools has no phase {phase}")))})
    }
    fn abort<'a>(&'a self,task:&'a P,_:&'a Rt<'a>,_:&'a Context)->LocalBoxFuture<'a,Result<AbortClosure<(),dyn CoreTx>>> {
        let inputs=task.input.inputs.clone();
        Box::pin(async move {
            let closure: AbortClosure<(),dyn CoreTx> = Box::new(move |tx,current,_|Box::pin(async move {
                clear_turn(tx,current.conversation_id)?;
                tx.resolve_inputs(&inputs,InputResolution::Unanswered {reason:InputReason::Aborted,detail:None}).await?;
                emit(tx,CoreViewEvent::TurnEnded {inputs,resolution:TurnResolution::Unanswered {reason:InputReason::Aborted,detail:None}})?;Ok(())
            }));
            Ok(closure)
        })
    }
}
fn successor(tx:&mut dyn CoreTx,inputs:Vec<Id>)->Result<Option<Id>> {
    if inputs.is_empty() {return Ok(None);}
    emit(tx,CoreViewEvent::TurnStarted {inputs:inputs.clone()})?;
    create(tx,"pi.generation",&GenerationInput {inputs},vec![]).map(Some)
}

#[cfg(test)]
#[path="post_tools_tests.rs"]
mod tests;
