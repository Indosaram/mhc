//! Durable turn generation; upstream `pico3/kinds/generation.ts`.
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use maho_ai::types::{ContentBlock, StopReason};
use super::super::types::*;
use super::collapse::choose_through;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationInput { pub inputs: Vec<Id> }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prep {
    pub cutoff: Id, pub system: Option<Id>, pub model: ModelRef,
    pub thinking_level: ThinkingLevel, pub tools: Vec<String>, pub retry: RetryPolicy,
    pub attempt: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum GenerationCheckpoint {
    Prepared { #[serde(flatten)] prep: Prep },
    Requesting { #[serde(flatten)] prep: Prep },
    Retrying { #[serde(flatten)] prep: Prep, until_ms: u64, last_error: String },
    Deferred { #[serde(flatten)] prep: Prep, handle: Stored<DeferredHandle>, poll_at: u64 },
}
impl GenerationCheckpoint {
    fn prep(&self) -> &Prep { match self {
        Self::Prepared { prep } | Self::Requesting { prep } | Self::Retrying { prep, .. } | Self::Deferred { prep, .. } => prep,
    } }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationResult {
    pub assistant: Id, pub tools: Vec<Id>,
    #[serde(skip_serializing_if = "Option::is_none")] pub post_tools: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")] pub successor: Option<Id>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenerationFailureData {
    pub reason: GenerationFailure, pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub assistant: Option<Id>,
}
pub type BeforeRequest = std::rc::Rc<dyn for<'a> Fn(Vec<RequestMessage>, HookInfo, Id, &'a Context) -> LocalBoxFuture<'a, Result<Option<Vec<RequestMessage>>>>>;
pub type AfterResponse = std::rc::Rc<dyn for<'a> Fn(AssistantMessage, HookInfo, u32, &'a Context) -> LocalBoxFuture<'a, Result<()>>>;
pub type OnYield = std::rc::Rc<dyn for<'a> Fn(AssistantMessage, HookInfo, &'a Context) -> LocalBoxFuture<'a, Result<Option<String>>>>;
#[derive(Default)]
pub struct GenerationHooks {
    pub before_request: Option<BeforeRequest>, pub after_response: Option<AfterResponse>, pub on_yield: Option<OnYield>,
    pub system_instructions: Option<super::super::system::SystemInstructionsHandler>,
}
type G = Task<GenerationInput, GenerationCheckpoint>;
type Rt = dyn Runtime<GenerationHooks, dyn CoreTx>;
type S = Step<GenerationCheckpoint, GenerationResult, GenerationFailureData, dyn CoreTx>;

pub(super) fn json<T: Serialize>(value: &T) -> Result<JsonValue> { Ok(to_stored(value)?.as_json().clone()) }
pub(super) fn decode<T: serde::de::DeserializeOwned>(value: &JsonValue) -> Result<T> { Stored::<T>::from_json(value.clone()).decode() }
pub(super) fn object<T: Serialize>(value: &T) -> Result<JsonObject> { decode(&json(value)?) }
pub(super) fn entry(kind: &str) -> NewEntry { NewEntry { kind: kind.into(), model: None, data: None, head: None, edits: None } }
pub(super) fn emit(tx: &mut dyn CoreTx, event: CoreViewEvent) -> Result<()> { tx.emit(ViewEvent::Core(event)) }
pub(super) fn create<T: Serialize>(tx: &mut dyn CoreTx, kind: &str, input: &T, after: Vec<Id>) -> Result<Id> {
    tx.create_task_spec(TaskSpec { kind: kind.into(), input: json(input)?, conversation_id: None, after: Some(after), background: None })
}
pub(super) async fn head(tx: &dyn CoreTx, c: Id) -> Result<Option<Id>> {
    Ok(tx.newest_entry(EntryScan { conversation_id: c, kind: None, with_head: Some(true), before: None, limit: 1 }).await?.map(|e|e.id))
}
pub(super) fn clear_turn(tx: &dyn CoreTx, c: Id) -> Result<()> { tx.sticky(c)?.set("turn", &json(&TurnState::default())?) }
pub(super) fn user(text: &str, now: u64) -> Result<StoredMessage> {
    Ok(StoredMessage::from_json(json(&serde_json::json!({"role":"user","content":text,"timestamp":now}))?))
}
pub(super) fn checkpoint<T: Serialize>(tx: &mut dyn CoreTx, cp: &T) -> Result<()> { tx.checkpoint(decode(&json(cp)?)?) }
fn failed(reason: GenerationFailure, detail: String, assistant: Option<Id>) -> Completion<GenerationResult, GenerationFailureData> {
    Completion::Failed { failure: GenerationFailureData { reason, detail, assistant } }
}
fn fail_step(inputs: Vec<Id>, reason: GenerationFailure, detail: String) -> S {
    Step::Done(Box::new(move |tx,current,_| Box::pin(async move {
        let boundary = head(tx,current.conversation_id).await?;
        emit(tx,CoreViewEvent::GenerationFailed { task_id:current.id,reason,detail:detail.clone(),entry:None })?;
        settle_failed(tx,current,&inputs,&detail,boundary).await?;
        Ok(failed(reason,detail,None))
    })))
}
async fn settle_failed(tx: &mut dyn CoreTx, current: &Task, inputs: &[Id], detail: &str, boundary: Option<Id>) -> Result<()> {
    clear_turn(tx,current.conversation_id)?;
    tx.resolve_inputs(inputs,InputResolution::Unanswered { reason:InputReason::Failed,detail:Some(detail.into()) }).await?;
    emit(tx,CoreViewEvent::TurnEnded { inputs:inputs.to_vec(),resolution:TurnResolution::Unanswered { reason:InputReason::Failed,detail:Some(detail.into()) } })?;
    let placed=tx.boundary(current.conversation_id,Boundary::Final,boundary).await?;
    if !placed.triggers.is_empty() {
        create(tx,"pi.generation",&GenerationInput { inputs:placed.triggers.clone() },vec![])?;
        emit(tx,CoreViewEvent::TurnStarted { inputs:placed.triggers })?;
    }
    Ok(())
}
#[derive(Debug, PartialEq, Eq)]
pub enum RetryDecision { Retry { until_ms:u64 }, Fail { exhausted:bool } }
pub fn retry_decision(retry: &RetryPolicy, attempt:u32, message:Option<&AssistantMessage>, now:u64) -> RetryDecision {
    if message.is_some_and(|m| !maho_ai::utils::retry::is_retryable_assistant_error(m)) || !retry.enabled {
        return RetryDecision::Fail { exhausted:false };
    }
    if attempt>retry.max_retries { return RetryDecision::Fail { exhausted:true }; }
    let delay=retry.base_delay_ms.checked_mul(2u64.checked_pow(attempt.saturating_sub(1)).unwrap_or(u64::MAX)).unwrap_or(u64::MAX).min(9_007_199_254_740_991);
    RetryDecision::Retry { until_ms:now.saturating_add(delay.min(retry.max_agent_delay_ms.unwrap_or(60_000))) }
}
pub(super) fn estimate(messages: &[StoredMessage], omit_errors:bool) -> Result<u64> {
    let mut native=Vec::new();
    for message in messages { match message.decode()? {
        Message::System(_) => {},
        Message::Assistant(m) => if !omit_errors || !matches!(m.stop_reason,StopReason::Error|StopReason::Aborted) { native.push(maho_ai::types::Message::Assistant(m)); },
        Message::User(m) => native.push(maho_ai::types::Message::User(m)),
        Message::ToolResult(m) => native.push(maho_ai::types::Message::ToolResult(m)),
    } }
    Ok(maho_ai::utils::estimate::estimate_context_tokens(&maho_ai::types::Context { messages:native,..Default::default() }).tokens)
}

pub struct GenerationKind {
    token:KindToken, config:KindConfig, inflight:Vec<String>,
    pub sections:std::rc::Rc<super::super::system::SectionRegistry>,
    pub tool_registry:std::rc::Rc<super::super::system::ToolRegistry>,
}
pub fn generation(sections:std::rc::Rc<super::super::system::SectionRegistry>,tool_registry:std::rc::Rc<super::super::system::ToolRegistry>) -> Result<GenerationKind> {
    Ok(GenerationKind { token:KindToken::core("pi.generation"),
        config:KindConfig { rewindable:object(&serde_json::json!({"thinkingLevel":"off","selectedTools":[],"profile":"default"}))?,
            sticky:object(&serde_json::json!({"retry":{"enabled":true,"maxRetries":3,"baseDelayMs":2000,"maxAgentDelayMs":60000}}))? },
        inflight:vec!["requesting".into()],sections,tool_registry })
}
impl GenerationKind {
    async fn prepare(&self,task:&G,rt:&Rt,ctx:&Context)->Result<S> {
        use super::super::system::{take_snapshot,prepare_draft,plan_managed_entry,same_snapshot,SystemInstructionsHook};
        let slot=std::rc::Rc::new(std::cell::RefCell::new(None));let output=slot.clone();
        let sections=self.sections.clone();let registry=self.tool_registry.clone();
        rt.commit(Box::new(move |tx,current,_|Box::pin(async move {
            let snapshot=take_snapshot(tx,current.conversation_id,&sections,&registry).await?;
            *output.borrow_mut()=Some(snapshot);Ok(JsonValue::Null)
        })),ctx).await?;
        let snapshot=slot.borrow_mut().take().ok_or_else(||Error::InvalidShape("snapshot callback did not return".into()))?;
        let Some(model)=snapshot.snapshot.settings.model.clone() else {
            return Ok(fail_step(task.input.inputs.clone(),GenerationFailure::NoModel,"no model configured".into()));
        };
        let retry=rt.sticky(task.conversation_id,ctx).await?.retry;
        let hooks:Vec<_>=rt.hooks().handlers().into_iter().filter_map(|binding|binding.handlers.system_instructions.clone().map(|handler|SystemInstructionsHook {handler,info:binding.api})).collect();
        let warnings=std::cell::RefCell::new(Vec::new());
        let prepared=prepare_draft(&hooks,&self.sections,&self.tool_registry,&snapshot.canonical,snapshot.seed.as_deref(),&snapshot.snapshot.settings,&|message|warnings.borrow_mut().push(message),ctx).await?;
        let warnings=warnings.into_inner();let now=rt.now();let sections=self.sections.clone();let registry=self.tool_registry.clone();
        Ok(Step::Build(Box::new(move |tx,current,_|Box::pin(async move {
            let c=current.conversation_id;let again=take_snapshot(tx,c,&sections,&registry).await?;
            if !same_snapshot(&snapshot.snapshot,&again.snapshot)? {return Ok(Transition::Retry);}
            let plan=plan_managed_entry(tx,c,&snapshot.snapshot,&snapshot.canonical,&prepared.desired,&prepared.tools,now).await?;
            let newest=if plan.is_none() {tx.newest_entry(EntryScan {conversation_id:c,kind:None,with_head:None,before:None,limit:1}).await?.map(|e|e.id)}else{None};
            for message in warnings {emit(tx,CoreViewEvent::Warning {source:"generation".into(),message})?;}
            let system=if let Some(plan)=plan {
                let mut e=entry("pi.system");e.model=Some(plan.model);e.data=Some(object(&plan.data)?);e.edits=plan.edits;
                Some(tx.append_entry(c,e)?)
            }else{None};
            let cutoff=system.or(newest).ok_or_else(||Error::InvalidShape("generation requires a request cutoff entry".into()))?;
            clear_turn(tx,c)?;
            Ok(Transition::Checkpoint(GenerationCheckpoint::Prepared {prep:Prep {
                cutoff,system,model,thinking_level:snapshot.snapshot.settings.thinking_level,
                tools:prepared.tools.iter().map(|tool|tool.name().into()).collect(),retry,attempt:0,
            }}))
        }))))
    }
}
impl Kind for GenerationKind {
    type Input=GenerationInput; type Checkpoint=GenerationCheckpoint; type Result=GenerationResult;
    type Failure=GenerationFailureData; type Aborted=JsonObject; type Hooks=GenerationHooks; type Slot=JsonObject; type Tx=dyn CoreTx;
    fn token(&self)->&KindToken { &self.token }
    fn turn(&self)->bool { true }
    fn config(&self)->&KindConfig { &self.config }
    fn inflight(&self)->&[String] { &self.inflight }
    fn slot(&self,_:&GenerationInput)->Result<JsonObject> { Ok(JsonObject::new()) }
    fn describe(&self,task:&G,_:Option<&JsonObject>)->Result<JsonValue> { json(&task.checkpoint) }
    fn initial<'a>(&'a self,task:&'a G,rt:&'a Rt,ctx:&'a Context)->LocalBoxFuture<'a,Result<S>> {
        Box::pin(async move { self.prepare(task,rt,ctx).await })
    }
    fn phase<'a>(&'a self,phase:&'a str,task:&'a G,rt:&'a Rt,ctx:&'a Context)->LocalBoxFuture<'a,Result<S>> {
        Box::pin(async move {
            let cp=task.checkpoint.as_ref().ok_or_else(||Error::InvalidShape("generation phase without checkpoint".into()))?;
            match (phase,cp) {
                ("prepared",GenerationCheckpoint::Prepared { prep }) => {
                    let Some(model)=rt.models().resolve(&prep.model) else { return Ok(fail_step(task.input.inputs.clone(),GenerationFailure::NoModel,"model unavailable".into())); };
                    let request=std::rc::Rc::new(std::cell::RefCell::new(rt.context(task.conversation_id,Some(prep.cutoff),ctx).await?.messages));
                    rt.hooks().each(ctx,|h,api| { let request=request.clone(); async move {
                        match &h.before_request { Some(f)=> { let messages=request.borrow().clone(); f(messages,api,prep.cutoff,ctx).await },None=>Ok(None) }
                    } },|value| { *request.borrow_mut()=value; false }).await?;
                    let messages=request.borrow().clone();
                    if estimate(&messages,true)?>model.context_window.saturating_sub(model.max_tokens) { return Ok(overflow(task.input.inputs.clone(),rt.now())); }
                    let mut prep=prep.clone(); prep.attempt=prep.attempt.saturating_add(1);
                    let saved=prep.clone(); let id=task.id;
                    rt.commit(Box::new(move |tx,_,_|Box::pin(async move {
                        checkpoint(tx,&GenerationCheckpoint::Requesting { prep:saved.clone() })?;
                        emit(tx,CoreViewEvent::GenerationStarted { task_id:id,attempt:saved.attempt })?;
                        Ok(JsonValue::Null)
                    })),ctx).await?;
                    let terminal=stream(&prep,&model,messages,rt,ctx).await?;
                    match terminal {
                        StreamResult::Deferred(handle)=>Ok(deferred_step(prep,handle,rt.now(),task.id)?),
                        StreamResult::Message(message)=>classify(task,prep,message,rt,ctx).await,
                    }
                },
                ("requesting",GenerationCheckpoint::Requesting { prep }) => Ok(retry_step(task.input.inputs.clone(),prep.clone(),None,"interrupted".into(),rt.now())?),
                ("retrying",GenerationCheckpoint::Retrying { prep,until_ms,.. }) => {
                    rt.sleep(*until_ms,ctx).await?; let prep=prep.clone();
                    Ok(Step::Build(Box::new(move |tx,current,_|Box::pin(async move {
                        turn(tx,current.conversation_id)?.delete("message")?;
                        Ok(Transition::Checkpoint(GenerationCheckpoint::Prepared { prep }))
                    }))))
                },
                ("deferred",GenerationCheckpoint::Deferred { prep,handle,poll_at }) => {
                    let Some(model)=rt.models().resolve(&prep.model) else { return Ok(fail_step(task.input.inputs.clone(),GenerationFailure::NoModel,"model disappeared".into())); };
                    rt.sleep(*poll_at,ctx).await?;
                    match rt.models().fetch_deferred(&model,&handle.decode()?,ctx).await? {
                        DeferredResult::Deferred(handle)=>deferred_step(prep.clone(),handle,rt.now(),task.id),
                        DeferredResult::Message(message)=> {
                            let stored=json(&message)?;
                            rt.commit(Box::new(move |tx,current,_|Box::pin(async move { turn(tx,current.conversation_id)?.set("message",&stored)?; Ok(JsonValue::Null) })),ctx).await?;
                            classify(task,prep.clone(),message,rt,ctx).await
                        }
                    }
                },
                _=>Err(Error::InvalidShape(format!("generation phase/checkpoint mismatch: {phase}"))),
            }
        })
    }
    fn abort<'a>(&'a self,task:&'a G,rt:&'a Rt,ctx:&'a Context)->LocalBoxFuture<'a,Result<AbortClosure<JsonObject,dyn CoreTx>>> {
        Box::pin(async move {
            if let Some(GenerationCheckpoint::Deferred { prep,handle,.. })=&task.checkpoint {
                if let Some(model)=rt.models().resolve(&prep.model) {
                    if let Err(error)=rt.models().cancel_deferred(&model,&handle.decode()?,ctx).await {
                        tracing::warn!(%error,"deferred generation cancellation failed");
                    }
                }
            }
            let inputs=task.input.inputs.clone(); let attempt=task.checkpoint.as_ref().map_or(0,|c|c.prep().attempt);
            Ok(Box::new(move |tx,current,_|Box::pin(async move {
                let partial=turn(tx,current.conversation_id)?.get("message")?.map(|m|m.value()).transpose()?;
                let mut result=JsonObject::new();
                if let Some(mut partial)=partial.map(|v|decode::<AssistantMessage>(&v)).transpose()? {
                    if !partial.content.is_empty() {
                        partial.stop_reason=StopReason::Aborted;
                        let mut e=entry("pi.assistant"); e.data=Some(object(&serde_json::json!({"attempt":attempt,"display":partial,"reason":"aborted"}))?);
                        result.insert("assistant".into(),json(&tx.append_entry(current.conversation_id,e)?)?);
                    }
                }
                clear_turn(tx,current.conversation_id)?;
                tx.resolve_inputs(&inputs,InputResolution::Unanswered { reason:InputReason::Aborted,detail:None }).await?;
                emit(tx,CoreViewEvent::TurnEnded { inputs,resolution:TurnResolution::Unanswered { reason:InputReason::Aborted,detail:None } })?;
                Ok(result)
            })) as AbortClosure<JsonObject,dyn CoreTx>)
        })
    }
}
pub(super) fn turn(tx:&dyn CoreTx,c:Id)->Result<super::super::membrane::Document> {
    tx.sticky(c)?.get("turn")?.ok_or_else(||Error::InvalidShape("missing turn document".into()))
}
enum StreamResult { Message(AssistantMessage), Deferred(DeferredHandle) }
async fn stream(prep:&Prep,model:&Model,messages:Vec<RequestMessage>,rt:&Rt,ctx:&Context)->Result<StreamResult> {
    use maho_ai::utils::assistant_message_frame::{AssistantMessageFrame,AssistantMessageFrameEncoder};
    let mut stream=rt.models().stream(model,RequestOptions { messages,thinking_level:prep.thinking_level.clone() },ctx);
    let mut encoder=AssistantMessageFrameEncoder::new(); let mut pending=Vec::new(); let mut bytes=0usize;
    let mut last=rt.now(); let mut flushed=false;
    loop {
        let event=if pending.is_empty() { stream.next().await } else {
            tokio::select! { event=stream.next()=>event, timer=rt.sleep(last.saturating_add(100),ctx)=> {
                timer?; flush_frames(rt,ctx,std::mem::take(&mut pending)).await?; bytes=0; last=rt.now(); continue;
            } }
        };
        let Some(event)=event else { return Err(Error::InvalidShape("stream ended without terminal message".into())); };
        let frame=encoder.encode(&event).map_err(|e|Error::InvalidShape(e.to_string()))?;
        match event {
            AssistantMessageEvent::Done { message,.. } => {
                flush_frames(rt,ctx,pending).await?;
                return Ok(match message.deferred.clone().filter(|_|message.stop_reason==StopReason::Deferred) { Some(h)=>StreamResult::Deferred(h),None=>StreamResult::Message(message) });
            },
            AssistantMessageEvent::Error { error,.. } => { flush_frames(rt,ctx,pending).await?; return Ok(StreamResult::Message(error)); },
            AssistantMessageEvent::Start {..}|AssistantMessageEvent::TextStart {..}|AssistantMessageEvent::TextDelta {..}|AssistantMessageEvent::TextEnd {..}|AssistantMessageEvent::ThinkingStart {..}|AssistantMessageEvent::ThinkingDelta {..}|AssistantMessageEvent::ThinkingEnd {..}|AssistantMessageEvent::ToolcallStart {..}|AssistantMessageEvent::ToolcallDelta {..}|AssistantMessageEvent::ToolcallEnd {..}=>{}
        }
        if let Some(frame)=frame {
            let delta=match &frame { AssistantMessageFrame::TextDelta {delta,..}|AssistantMessageFrame::ThinkingDelta {delta,..}|AssistantMessageFrame::ToolcallDelta {delta,..}=>Some(delta.encode_utf16().count()),_=>None };
            bytes=bytes.saturating_add(delta.unwrap_or(64)); pending.push(frame);
            if bytes>=256 || (!flushed && delta.is_some()) {
                flushed|=delta.is_some(); flush_frames(rt,ctx,std::mem::take(&mut pending)).await?; bytes=0; last=rt.now();
            }
        }
    }
}
async fn flush_frames(rt:&Rt,ctx:&Context,frames:Vec<maho_ai::utils::assistant_message_frame::AssistantMessageFrame>)->Result<()> {
    if frames.is_empty() { return Ok(()); }
    rt.commit(Box::new(move |tx,current,_|Box::pin(async move {
        let turn=turn(tx,current.conversation_id)?; let mut output:JsonObject=decode(&turn.value()?)?;
        for frame in &frames { super::frames::apply_frame(&mut output,frame)?; }
        tx.sticky(current.conversation_id)?.set("turn",&json(&output)?)?; Ok(JsonValue::Null)
    })),ctx).await?; Ok(())
}
fn deferred_step(prep:Prep,handle:DeferredHandle,now:u64,id:Id)->Result<S> {
    let poll_at=now.saturating_add(handle.poll_after_ms.unwrap_or(5000)); let handle=to_stored(&handle)?;
    Ok(Step::Build(Box::new(move |tx,_,_|Box::pin(async move {
        emit(tx,CoreViewEvent::GenerationDeferred { task_id:id,poll_at })?;
        Ok(Transition::Checkpoint(GenerationCheckpoint::Deferred { prep,handle,poll_at }))
    }))))
}
fn retry_step(inputs:Vec<Id>,prep:Prep,message:Option<AssistantMessage>,detail:String,now:u64)->Result<S> {
    match retry_decision(&prep.retry,prep.attempt,message.as_ref(),now) {
        RetryDecision::Fail { exhausted }=>Ok(fail_step(inputs,if exhausted {GenerationFailure::RetriesExhausted}else{GenerationFailure::Provider},detail)),
        RetryDecision::Retry { until_ms }=>Ok(Step::Build(Box::new(move |tx,current,_|Box::pin(async move {
            let mut e=entry("pi.usage"); let mut data=object(&serde_json::json!({"attempt":prep.attempt,"error":detail}))?;
            if let Some(message)=message { data.insert("usage".into(),json(&message.usage)?); }
            e.data=Some(data); tx.append_entry(current.conversation_id,e)?;
            emit(tx,CoreViewEvent::GenerationRetrying {task_id:current.id,attempt:prep.attempt,retry_at:until_ms,error:detail.clone()})?;
            Ok(Transition::Checkpoint(GenerationCheckpoint::Retrying {prep,until_ms,last_error:detail}))
        })))),
    }
}
async fn classify(task:&G,prep:Prep,message:AssistantMessage,rt:&Rt,ctx:&Context)->Result<S> {
    let attempt=prep.attempt;
    rt.hooks().each::<(),_,_>(ctx,|h,api| { let message=message.clone(); async move {
        if let Some(f)=&h.after_response { f(message,api,attempt,ctx).await?; } Ok(None)
    } },|()|false).await?;
    if message.stop_reason==StopReason::Error {
        let detail=message.error_message.clone().unwrap_or_else(||"provider error".into());
        if matches!(retry_decision(&prep.retry,prep.attempt,Some(&message),rt.now()),RetryDecision::Retry {..}) {
            return retry_step(task.input.inputs.clone(),prep,Some(message),detail,rt.now());
        }
    }
    if matches!(message.stop_reason,StopReason::Error|StopReason::Aborted) {
        let reason=match retry_decision(&prep.retry,prep.attempt,Some(&message),rt.now()) {
            RetryDecision::Fail {exhausted:true}=>GenerationFailure::RetriesExhausted,
            RetryDecision::Fail {exhausted:false}|RetryDecision::Retry {..}=>GenerationFailure::Provider,
        };
        let inputs=task.input.inputs.clone();
        return Ok(Step::Done(Box::new(move |tx,current,_|Box::pin(async move {
            let boundary=head(tx,current.conversation_id).await?;
            let detail=message.error_message.clone().unwrap_or_else(||"provider error".into());
            let mut e=entry("pi.assistant"); e.data=Some(object(&serde_json::json!({"attempt":prep.attempt,"display":message,"reason":if message.stop_reason==StopReason::Aborted {"aborted"} else {"error"}}))?);
            let assistant=tx.append_entry(current.conversation_id,e)?;
            emit(tx,CoreViewEvent::GenerationFailed {task_id:current.id,reason,detail:detail.clone(),entry:Some(assistant)})?;
            settle_failed(tx,current,&inputs,&detail,boundary).await?;
            Ok(failed(reason,detail,Some(assistant)))
        }))));
    }
    let calls:Vec<_>=message.content.iter().filter_map(|b|match b {ContentBlock::ToolCall(call)=>Some(call.clone()),_=>None}).collect();
    let continuation=if calls.is_empty() {
        rt.hooks().first(ctx,|h,api| {let message=message.clone(); async move {match &h.on_yield {Some(f)=>f(message,api,ctx).await,None=>Ok(None)}}}).await?
    } else {None};
    let inputs=task.input.inputs.clone(); let now=rt.now();
    Ok(Step::Done(Box::new(move |tx,current,_|Box::pin(async move {
        let c=current.conversation_id;
        let through=threshold_through(tx,c,&message).await?;
        let boundary=head(tx,c).await?;
        let mut e=entry("pi.assistant"); e.model=Some(vec![StoredMessage::from_json(json(&message)?)]); e.data=Some(object(&serde_json::json!({"attempt":prep.attempt}))?);
        let assistant=tx.append_entry(c,e)?;
        emit(tx,CoreViewEvent::GenerationCompleted {task_id:current.id,entry:assistant,tool_calls:calls.len()})?;
        let mut result=GenerationResult {assistant,tools:vec![],post_tools:None,successor:None};
        if !calls.is_empty() {
            turn(tx,c)?.delete("message")?;
            let slots:Vec<_>=calls.iter().map(|call|serde_json::json!({"callId":call.id,"name":call.name,"args":call.arguments,"status":"pending"})).collect();
            turn(tx,c)?.set("tools",&json(&slots)?)?;
            for (index,call) in calls.iter().enumerate() {
                result.tools.push(create(tx,"pi.tool",&serde_json::json!({"assistant":assistant,"call":call,"offered":prep.tools,"index":index}),vec![])?);
            }
            result.post_tools=Some(create(tx,"pi.post_tools",&super::post_tools::PostToolsInput {inputs,assistant,tools:result.tools.clone()},result.tools.clone())?);
            if let Some(through)=through { create(tx,"pi.collapse",&super::collapse::CollapseInput {reason:CompactionReason::Threshold,through,instructions:None},vec![])?; }
            return Ok(Completion::Completed {result});
        }
        clear_turn(tx,c)?;
        if let Some(through)=through {create(tx,"pi.collapse",&super::collapse::CollapseInput {reason:CompactionReason::Threshold,through,instructions:None},vec![])?;}
        let placed=tx.boundary(c,Boundary::Final,boundary).await?;
        if let Some(text)=continuation.filter(|_|placed.triggers.is_empty()&&!placed.terminated) {
            let mut e=entry("pi.user");e.model=Some(vec![user(&text,now)?]);e.data=Some(object(&serde_json::json!({"continuation":true,"from":assistant}))?);tx.append_entry(c,e)?;
            result.successor=Some(create(tx,"pi.generation",&GenerationInput {inputs},vec![])?);
            return Ok(Completion::Completed {result});
        }
        tx.resolve_inputs(&inputs,InputResolution::Done {answer:assistant}).await?;
        emit(tx,CoreViewEvent::TurnEnded {inputs,resolution:TurnResolution::Done {answer:assistant}})?;
        if !placed.triggers.is_empty() {
            result.successor=Some(create(tx,"pi.generation",&GenerationInput {inputs:placed.triggers.clone()},vec![])?);
            emit(tx,CoreViewEvent::TurnStarted {inputs:placed.triggers})?;
        }
        Ok(Completion::Completed {result})
    }))))
}
async fn threshold_through(tx:&dyn CoreTx,c:Id,message:&AssistantMessage)->Result<Option<Id>> {
    if !tx.tasks(TaskScan {conversation_id:Some(c),status:Some(vec![TaskStatus::Pending,TaskStatus::Running]),kind:Some("pi.collapse".into())}).await?.is_empty() {return Ok(None);}
    let state:RewindableState=decode(&tx.rewindable(c)?.value()?)?;
    if state.threshold<=0.0 {return Ok(None);}
    let context=tx.context(c,None).await?; let mut messages=context.messages;
    messages.push(StoredMessage::from_json(json(message)?));
    let used=message.usage.input.saturating_add(message.usage.output).max(estimate(&messages,true)?);
    if (used as f64)<=state.threshold {return Ok(None);}
    choose_through(&context.entries,state.keep_recent)
}
fn overflow(inputs:Vec<Id>,now:u64)->S {
    Step::Done(Box::new(move |tx,current,_|Box::pin(async move {
        let c=current.conversation_id;let state:RewindableState=decode(&tx.rewindable(c)?.value()?)?;
        let context=tx.context(c,None).await?;let through=choose_through(&context.entries,state.keep_recent)?;
        clear_turn(tx,c)?;
        let detail=match through {
            Some(through)=> {
                let collapse=create(tx,"pi.collapse",&super::collapse::CollapseInput {reason:CompactionReason::Overflow,through,instructions:None},vec![])?;
                create(tx,"pi.generation",&GenerationInput {inputs},vec![collapse])?;
                format!("collapsing through {through}")
            },
            None=> {
                let mut notice=entry("pi.notice");notice.model=Some(vec![user("Context too large; nothing to compact.",now)?]);tx.write(c,notice).await?;
                settle_failed(tx,current,&inputs,"overflow",context.head.map(|e|e.id)).await?;
                "request exceeds context window and nothing is collapsible".into()
            }
        };
        emit(tx,CoreViewEvent::GenerationFailed {task_id:current.id,reason:GenerationFailure::Overflow,detail:detail.clone(),entry:None})?;
        Ok(failed(GenerationFailure::Overflow,detail,None))
    })))
}

#[cfg(test)]
#[path="generation_tests.rs"]
pub(crate) mod tests;
