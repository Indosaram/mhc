use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{EvalHandleCallContext, EvalHandleHost, EvalHandleKind, EvalHandleOutputRequest, EvalHandleRef, EvalHandleSnapshot, EvalHandleUpdateStream, EvalHandleWatch, ExtensionFailure};
use serde_json::{Value, json};

pub const RESERVED_WAIT_TOOL: &str = crate::bridge::reserved::RESERVED_WAIT_TOOL;
pub const RESERVED_HANDLE_STATUS_TOOL: &str = crate::bridge::reserved::RESERVED_HANDLE_STATUS_TOOL;
pub const RESERVED_HANDLE_OUTPUT_TOOL: &str = crate::bridge::reserved::RESERVED_HANDLE_OUTPUT_TOOL;
pub const RESERVED_HANDLE_SEND_TOOL: &str = crate::bridge::reserved::RESERVED_HANDLE_SEND_TOOL;
pub const RESERVED_HANDLE_CANCEL_TOOL: &str = crate::bridge::reserved::RESERVED_HANDLE_CANCEL_TOOL;
pub fn is_handle_tool_name(name:&str)->bool { matches!(name,RESERVED_WAIT_TOOL|RESERVED_HANDLE_STATUS_TOOL|RESERVED_HANDLE_OUTPUT_TOOL|RESERVED_HANDLE_SEND_TOOL|RESERVED_HANDLE_CANCEL_TOOL) }

#[derive(Debug, thiserror::Error)]
pub enum EvalHandleBridgeError {
    #[error("eval handle call received invalid arguments: {0}")]
    Arguments(String),
    #[error("eval handle unavailable: no registered host")]
    Unavailable,
    #[error(transparent)] Host(#[from] ExtensionFailure),
}

pub struct EvalHandleBridgeOptions<'a> {
    pub host: Option<&'a dyn EvalHandleHost>,
    pub completions: &'a std::sync::Arc<crate::bridges::handles::CompletionHandleRegistry>,
    pub owner_session_id: &'a str,
    pub signal: Option<AbortSignal>,
}

impl EvalHandleBridgeOptions<'_> {
    fn context(&self) -> EvalHandleCallContext {
        EvalHandleCallContext { owner_session_id: self.owner_session_id.to_owned(), signal: self.signal.clone() }
    }
}

pub fn parse_handle_reference(value: &Value) -> Result<EvalHandleRef, EvalHandleBridgeError> {
    let object = value.as_object().ok_or_else(|| EvalHandleBridgeError::Arguments("handle ref must be an object".into()))?;
    let kind = match object.get("kind").and_then(Value::as_str) {
        Some("agent") => EvalHandleKind::Agent, Some("completion") => EvalHandleKind::Completion,
        Some("workpool") => EvalHandleKind::Workpool,
        _ => return Err(EvalHandleBridgeError::Arguments("/kind must be agent, completion, or workpool".into())),
    };
    let id = object.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).ok_or_else(|| EvalHandleBridgeError::Arguments("/id must be a nonempty string".into()))?;
    let run_epoch = object.get("run_epoch").and_then(Value::as_i64).filter(|epoch| *epoch >= 0).ok_or_else(|| EvalHandleBridgeError::Arguments("/run_epoch must be a nonnegative integer".into()))?;
    Ok(EvalHandleRef { kind, id:id.into(), run_epoch })
}

fn reference_value(reference: &EvalHandleRef) -> Value {
    json!({"kind":match reference.kind { EvalHandleKind::Agent=>"agent", EvalHandleKind::Completion=>"completion", EvalHandleKind::Workpool=>"workpool" },"id":reference.id,"run_epoch":reference.run_epoch})
}

pub async fn run_handle_tool(name: &str, args: &Value, options: EvalHandleBridgeOptions<'_>) -> Result<Value, EvalHandleBridgeError> {
    let object=args.as_object().ok_or_else(||EvalHandleBridgeError::Arguments("Expected object".into()))?;
    let context=options.context();
    match name {
        RESERVED_HANDLE_STATUS_TOOL => {
            let reference=parse_handle_reference(object.get("ref").ok_or_else(||EvalHandleBridgeError::Arguments("/ref required".into()))?)?;
            let watch=watch_handles(&options, vec![reference], context).await?;
            let value=watch.initial.first().cloned().ok_or_else(||EvalHandleBridgeError::Arguments("no handle status snapshot".into()))?;
            (watch.close)();
            Ok(snapshot_value(&value))
        }
        RESERVED_HANDLE_OUTPUT_TOOL => {
            let reference=parse_handle_reference(object.get("ref").ok_or_else(||EvalHandleBridgeError::Arguments("/ref required".into()))?)?;
            let format=object.get("format").and_then(Value::as_str);
            if format.is_some_and(|value|!matches!(value,"raw"|"tail")){return Err(EvalHandleBridgeError::Arguments("/format must be raw or tail".into()));}
            let request=EvalHandleOutputRequest {tail:format==Some("tail"),offset:integer_option(object.get("offset"),"/offset")?,limit:integer_option(object.get("limit"),"/limit")?};
            let value=if reference.kind==EvalHandleKind::Completion { completion_output(options.completions,&reference,request)? } else { options.host.ok_or(EvalHandleBridgeError::Unavailable)?.output(reference,request,context).await? };
            Ok(json!({"ref":reference_value(&value.reference),"text":value.text,"offset":value.offset,"total":value.total,"truncated":value.truncated}))
        }
        RESERVED_HANDLE_SEND_TOOL => {
            let reference=parse_handle_reference(object.get("ref").ok_or_else(||EvalHandleBridgeError::Arguments("/ref required".into()))?)?;
            let message=object.get("message").and_then(Value::as_str).filter(|message|!message.is_empty()).ok_or_else(||EvalHandleBridgeError::Arguments("/message must be a nonempty string".into()))?;
            if reference.kind==EvalHandleKind::Completion { return Err(EvalHandleBridgeError::Arguments("send() is for agent handles only".into())); }
            let value=options.host.ok_or(EvalHandleBridgeError::Unavailable)?.send(reference,message.into(),context).await?;
            Ok(snapshot_value(&value))
        }
        RESERVED_HANDLE_CANCEL_TOOL => {
            let reference=parse_handle_reference(object.get("ref").ok_or_else(||EvalHandleBridgeError::Arguments("/ref required".into()))?)?;
            if reference.kind==EvalHandleKind::Completion { let value=options.completions.cancel(&reference).map_err(|error|EvalHandleBridgeError::Arguments(format!("{}: {}",error.code,error.message)))?;return Ok(json!({"ref":reference_value(&value.reference),"cancelled":value.cancelled,"phase":phase(&value.phase)})); }
            let value=options.host.ok_or(EvalHandleBridgeError::Unavailable)?.cancel(reference,context).await?;
            Ok(json!({"ref":reference_value(&value.reference),"cancelled":value.cancelled,"phase":phase(&value.phase)}))
        }
        RESERVED_WAIT_TOOL => wait_handles(object,options).await,
        _=>Err(EvalHandleBridgeError::Arguments(format!("unknown reserved handle tool {name}"))),
    }
}

fn phase(value:&maho_ext_api::EvalHandlePhase)->&'static str { match value { maho_ext_api::EvalHandlePhase::Pending=>"pending",maho_ext_api::EvalHandlePhase::Succeeded=>"succeeded",maho_ext_api::EvalHandlePhase::Failed=>"failed",maho_ext_api::EvalHandlePhase::Cancelled=>"cancelled",maho_ext_api::EvalHandlePhase::Lost=>"lost" } }
fn snapshot_value(value:&EvalHandleSnapshot)->Value { json!({"ref":reference_value(&value.reference),"phase":phase(&value.phase),"host_status":value.host_status,"revision":value.revision}) }

fn integer_option(value: Option<&Value>, path: &str) -> Result<Option<usize>, EvalHandleBridgeError> {
    let Some(value) = value else { return Ok(None); };
    value.as_u64().and_then(|number| usize::try_from(number).ok()).map(Some).ok_or_else(|| EvalHandleBridgeError::Arguments(format!("{path} must be a nonnegative integer")))
}

async fn wait_handles(object:&serde_json::Map<String,Value>,options:EvalHandleBridgeOptions<'_>)->Result<Value,EvalHandleBridgeError>{
    let refs=object.get("refs").and_then(Value::as_array).ok_or_else(||EvalHandleBridgeError::Arguments("/refs must be an array".into()))?.iter().map(parse_handle_reference).collect::<Result<Vec<_>,_>>()?;
    let mode=object.get("mode").and_then(Value::as_str).unwrap_or("all");
    if !matches!(mode,"all"|"any"|"settled"){return Err(EvalHandleBridgeError::Arguments("/mode must be all, any, or settled".into()));}
    let timeout=object.get("timeout").and_then(Value::as_f64);
    if object.contains_key("timeout")&&timeout.is_none_or(|value|!value.is_finite()||value<0.0){return Err(EvalHandleBridgeError::Arguments("/timeout must be a finite number >= 0".into()));}
    if refs.is_empty(){return if mode=="any"{Err(EvalHandleBridgeError::Arguments("wait(any) needs at least one handle".into()))}else{Ok(json!([]))};}
    let watch=watch_handles(&options,refs.clone(),options.context()).await?;
    let maho_ext_api::EvalHandleWatch {initial,updates,close}=watch;
    let mut revisions=std::collections::HashMap::<EvalHandleRef,i64>::new();
    let mut terminal=std::collections::HashMap::<EvalHandleRef,Value>::new();
    let mut updates=updates;
    let deadline=timeout.map(|seconds|tokio::time::Instant::now()+std::time::Duration::from_secs_f64(seconds));
    let unique=refs.iter().cloned().collect::<std::collections::BTreeSet<_>>();let unique_count=unique.len();
    let result=async {
        for snapshot in initial { if absorb_snapshot(snapshot,&mut revisions,&mut terminal,&options,mode).await? && mode=="any" { return wait_result(&refs,mode,&terminal); } }
        if timeout==Some(0.0){return Err(EvalHandleBridgeError::Arguments(format!("wait() timed out after 0s; {}/{} handles settled; work was not cancelled",terminal.len(),refs.len())));}
        loop {
            if terminal.len()==unique_count{return wait_result(&refs,mode,&terminal);}
            let next=std::future::poll_fn(|context|updates.as_mut().poll_next(context));tokio::pin!(next);
            let snapshot=if let Some(deadline)=deadline {tokio::select!{biased;()=async{if let Some(signal)=&options.signal{signal.cancelled().await}else{std::future::pending().await}}=>return Err(EvalHandleBridgeError::Arguments("wait() cancelled with its cell".into())),_ = tokio::time::sleep_until(deadline)=>return Err(EvalHandleBridgeError::Arguments("wait() timed out; work was not cancelled".into())),snapshot=&mut next=>snapshot}} else if let Some(signal)=&options.signal {tokio::select!{biased;()=signal.cancelled()=>return Err(EvalHandleBridgeError::Arguments("wait() cancelled with its cell".into())),snapshot=&mut next=>snapshot}} else {next.await};
            let Some(snapshot)=snapshot else{return wait_result(&refs,mode,&terminal);};
            if absorb_snapshot(snapshot,&mut revisions,&mut terminal,&options,mode).await?&&mode=="any"{return wait_result(&refs,mode,&terminal);}
        }
    }.await;
    close();result
}

async fn absorb_snapshot(snapshot:EvalHandleSnapshot,revisions:&mut std::collections::HashMap<EvalHandleRef,i64>,terminal:&mut std::collections::HashMap<EvalHandleRef,Value>,options:&EvalHandleBridgeOptions<'_>,mode:&str)->Result<bool,EvalHandleBridgeError>{
    let reference=snapshot.reference.clone();if revisions.get(&reference).is_some_and(|revision|snapshot.revision<=*revision){return Ok(false)}revisions.insert(reference.clone(),snapshot.revision);
    if snapshot.phase==maho_ext_api::EvalHandlePhase::Pending{return Ok(false)}
    let outcome=if reference.kind==EvalHandleKind::Completion { options.completions.result(&reference).map_err(|error|EvalHandleBridgeError::Arguments(format!("{}: {}",error.code,error.message)))? } else { options.host.ok_or(EvalHandleBridgeError::Unavailable)?.result(reference.clone(),options.context()).await? };
    let value=match outcome{maho_ext_api::EvalHandleOutcome::Fulfilled{value,..}=>json!({"status":"fulfilled","ref":reference_value(&reference),"value":value}),maho_ext_api::EvalHandleOutcome::Rejected{error,..}=>json!({"status":"rejected","ref":reference_value(&reference),"error":{"code":error.code,"message":error.message,"details":error.details}})};
    let succeeded=value["status"]=="fulfilled";let rejected=value["status"]=="rejected";terminal.insert(reference.clone(),value.clone());
    if mode=="all"&&rejected{return Err(EvalHandleBridgeError::Arguments(format!("{}: {}",value["error"]["code"].as_str().unwrap_or("eval_handle_rejected"),value["error"]["message"].as_str().unwrap_or("handle rejected"))));}
    Ok(succeeded)
}

async fn watch_handles(options:&EvalHandleBridgeOptions<'_>,references:Vec<EvalHandleRef>,context:EvalHandleCallContext)->Result<EvalHandleWatch,EvalHandleBridgeError>{
    let local=references.iter().filter(|reference|reference.kind==EvalHandleKind::Completion).cloned().collect::<Vec<_>>();
    let remote=references.iter().filter(|reference|reference.kind!=EvalHandleKind::Completion).cloned().collect::<Vec<_>>();
    let mut initial=Vec::new();
    let mut closers=Vec::new();
    let mut streams:Vec<std::pin::Pin<Box<dyn EvalHandleUpdateStream>>>=Vec::new();
    if !local.is_empty(){let watch=options.completions.watch(&local).map_err(|error|EvalHandleBridgeError::Arguments(format!("{}: {}",error.code,error.message)))?;initial.extend(watch.initial);streams.push(watch.updates);closers.push(watch.close);}
    if !remote.is_empty(){
        let result=match options.host { Some(host)=>host.watch(remote,context).await.map_err(EvalHandleBridgeError::Host),None=>Err(EvalHandleBridgeError::Unavailable) };
        let watch=match result { Ok(watch)=>watch,Err(error)=>{for close in &closers{close();}return Err(error);} };
        initial.extend(watch.initial);streams.push(watch.updates);closers.push(watch.close);
    }
    let snapshots=references.iter().map(|reference|initial.iter().find(|snapshot|snapshot.reference==*reference).cloned().ok_or_else(||EvalHandleBridgeError::Arguments(format!("no snapshot for {}",reference.id)))).collect::<Result<Vec<_>,_>>()?;
    Ok(EvalHandleWatch{initial:snapshots,updates:Box::pin(MergedUpdates{streams}),close:std::sync::Arc::new(move ||{for close in &closers{close();}})})
}

struct MergedUpdates{streams:Vec<std::pin::Pin<Box<dyn EvalHandleUpdateStream>>>}
impl EvalHandleUpdateStream for MergedUpdates{
    fn poll_next(mut self:std::pin::Pin<&mut Self>,context:&mut std::task::Context<'_>)->std::task::Poll<Option<EvalHandleSnapshot>>{
        let mut index=0;
        while index<self.streams.len(){match self.streams[index].as_mut().poll_next(context){std::task::Poll::Ready(Some(snapshot))=>return std::task::Poll::Ready(Some(snapshot)),std::task::Poll::Ready(None)=>{drop(self.streams.remove(index));},std::task::Poll::Pending=>index+=1}}
        if self.streams.is_empty(){std::task::Poll::Ready(None)}else{std::task::Poll::Pending}
    }
}

fn completion_output(registry:&crate::bridges::handles::CompletionHandleRegistry,reference:&EvalHandleRef,request:EvalHandleOutputRequest)->Result<maho_ext_api::EvalHandleOutputSnapshot,EvalHandleBridgeError>{
    let text=registry.output_text(reference).map_err(|error|EvalHandleBridgeError::Arguments(format!("{}: {}",error.code,error.message)))?;
    let lines=if text.is_empty(){vec![]}else{text.split('\n').collect::<Vec<_>>()};let total=lines.len();let offset=if request.tail{total.saturating_sub(request.limit.unwrap_or(20))}else{request.offset.unwrap_or(0)};let start=offset.min(total);let end=if request.tail{total}else{start.saturating_add(request.limit.unwrap_or(total)).min(total)};let text=lines[start..end].join("\n");
    Ok(maho_ext_api::EvalHandleOutputSnapshot{reference:reference.clone(),text,offset,total,truncated:end-start<total})
}

fn wait_result(refs:&[EvalHandleRef],mode:&str,terminal:&std::collections::HashMap<EvalHandleRef,Value>)->Result<Value,EvalHandleBridgeError>{
    if mode=="any"{for(index,reference)in refs.iter().enumerate(){if let Some(outcome)=terminal.get(reference).filter(|outcome|outcome["status"]=="fulfilled"){return Ok(json!({"index":index,"ref":reference_value(reference),"value":outcome["value"]}));}}}
    let outcomes=refs.iter().filter_map(|reference|terminal.get(reference).cloned()).collect::<Vec<_>>();
    if mode=="all"&&let Some(rejected)=outcomes.iter().find(|outcome|outcome["status"]=="rejected") { return Err(EvalHandleBridgeError::Arguments(format!("{}: {}",rejected["error"]["code"].as_str().unwrap_or("eval_handle_rejected"),rejected["error"]["message"].as_str().unwrap_or("handle rejected")))); }
    if mode=="any" {return Err(EvalHandleBridgeError::Arguments(format!("eval_wait_all_rejected: wait(any) found no successful handle among {}; {}",outcomes.len(),outcomes.iter().map(|outcome|format!("{}: {}",outcome["ref"]["id"].as_str().unwrap_or(""),outcome["error"]["code"].as_str().unwrap_or("eval_handle_rejected"))).collect::<Vec<_>>().join("; "))));}
    if mode=="all" { Ok(Value::Array(outcomes.into_iter().map(|outcome|outcome["value"].clone()).collect())) } else { Ok(Value::Array(outcomes)) }
}


