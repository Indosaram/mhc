#[derive(Debug,thiserror::Error)]pub enum RpcModeOutputError{
    #[error(transparent)]Json(#[from]serde_json::Error),
    #[error(transparent)]Event(#[from]crate::json_event::JsonEventError),
}
pub fn linearize_output(chunk:&str)->Result<String,RpcModeOutputError>{
    let mut output=String::new();
    for line in chunk.split('\n').filter(|line|!line.is_empty()){
        let value:serde_json::Value=serde_json::from_str(line)?;
        let value=crate::json_event::to_json_event(&value)?;
        output.push_str(&crate::jsonl::serialize_json_line(&value)?);
    }
    Ok(output)
}
async fn write_event(output:&mut(impl tokio::io::AsyncWrite+Unpin),event:Result<serde_json::Value,serde_json::Error>)->std::io::Result<()>{
    use tokio::io::AsyncWriteExt;
    let record=event.map_err(std::io::Error::other)?;
    let wire=crate::json_event::to_json_event(&record).map_err(std::io::Error::other)?;
    output.write_all(crate::jsonl::serialize_json_line(&wire)?.as_bytes()).await
}
async fn finish_command(output:&mut(impl tokio::io::AsyncWrite+Unpin),events:&mut tokio::sync::mpsc::UnboundedReceiver<Result<serde_json::Value,serde_json::Error>>,response:Result<Option<String>,serde_json::Error>)->std::io::Result<()>{
    use tokio::io::AsyncWriteExt;
    // Capture a finite prefix. Awaited writes may synchronously enqueue more events.
    let pending=events.len();
    for _ in 0..pending{let Ok(event)=events.try_recv() else{break;};write_event(output,event).await?;}
    if let Some(response)=response?{output.write_all(response.as_bytes()).await?;}
    Ok(())
}
async fn pump(handler:&crate::connection_handler::RpcConnectionHandler,mut input:impl tokio::io::AsyncRead+Unpin,output:&mut(impl tokio::io::AsyncWrite+Unpin),events:&mut tokio::sync::mpsc::UnboundedReceiver<Result<serde_json::Value,serde_json::Error>>,signal:&mut(impl std::future::Future<Output=i32>+Unpin))->std::io::Result<i32>{
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    use std::{future::Future,pin::Pin,task::Poll};
    type CommandFuture<'a>=Pin<Box<dyn Future<Output=Result<Option<String>,serde_json::Error>>+'a>>;
    let mut commands:Vec<CommandFuture<'_>>=Vec::new();
    let mut reader=crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS).expect("positive line limit");
    let mut bytes=[0u8;8192];
    let mut eof=false;
    loop{
        // Poll newly received commands before handling EOF, including a final unterminated line.
        let completed=std::future::poll_fn(|cx|{
            for(index,command)in commands.iter_mut().enumerate(){if let Poll::Ready(response)=command.as_mut().poll(cx){return Poll::Ready(Some((index,response)));}}
            Poll::Ready(None)
        }).await;
        if let Some((index,response))=completed{drop(commands.remove(index));finish_command(output,events,response).await?;if handler.is_shutdown_requested(){return Ok(0);}continue;}
        // Dropping this owned future drops every pending command and the sole writer.
        if eof{finish_command(output,events,Ok(None)).await?;return Ok(0);}
        let count=tokio::select!{
            code=&mut *signal=>return Ok(code),
            event=events.recv()=>{let event=event.ok_or_else(||std::io::Error::other("RPC event channel closed while subscribed"))?;write_event(output,event).await?;continue;},
            completed=std::future::poll_fn(|cx|{
                for(index,command)in commands.iter_mut().enumerate(){if let Poll::Ready(response)=command.as_mut().poll(cx){return Poll::Ready((index,response));}}
                Poll::Pending
            }),if !commands.is_empty()=>{let(index,response)=completed;drop(commands.remove(index));finish_command(output,events,response).await?;if handler.is_shutdown_requested(){return Ok(0);}continue;},
            count=input.read(&mut bytes)=>count?,
        };
        let records=if count==0{reader.finish()}else{reader.push(&bytes[..count])};
        for record in records{
            match record{
                crate::jsonl::LineRecord::Line(line)=>{
                    commands.push(Box::pin(async move{handler.handle_input_line(&line).await}));
                },
                crate::jsonl::LineRecord::Oversized=>{
                    let response=crate::jsonl::serialize_json_line(&serde_json::json!({"type":"response","command":"parse","success":false,"error":format!("RPC input line exceeds {} characters.",crate::jsonl::MAX_RPC_LINE_CHARACTERS)}))?;
                    output.write_all(response.as_bytes()).await?;
                }
            }
        }
        eof=count==0;
    }
}
/// One connection over supplied streams: the handler owns extension UI, capability-gated events
/// and the command loop; the returned code is 0 for EOF or an extension-requested shutdown, or the
/// code `signal` resolved with.
async fn serve(session:&maho_core::agent_session::AgentSession,capabilities:Vec<String>,input:impl tokio::io::AsyncRead+Unpin,output:&mut(impl tokio::io::AsyncWrite+Unpin),signal:&mut(impl std::future::Future<Output=i32>+Unpin))->(crate::connection_handler::RpcConnectionHandler,std::io::Result<i32>){
    let(events_tx,mut events)=tokio::sync::mpsc::unbounded_channel();
    let sender=events_tx.clone();
    let last_abort_source:crate::connection_handler::LastAbortSource=Default::default();
    let (event_session,event_abort_source)=(session.weak_accessor(),last_abort_source.clone());
    let subscription=session.subscribe(std::sync::Arc::new(move|event|{
        // `write_event` keeps only what `to_json_event` reads, so a `message_update` skips
        // serializing the cumulative `message`/`partial` snapshots it would drop.
        if let maho_ext_api::AgentSessionEvent::Agent(update@maho_ext_api::AgentEvent::MessageUpdate{message,assistant_message_event})=event
            &&let Some(mut record)=crate::print_mode::stripped_message_update(message,assistant_message_event){
            if let Ok(record)=&mut record&&let Some(name)=event_session().and_then(|session|session.message_update_resolved_tool_name(update)){record["resolvedToolName"]=name.into();}
            let _=sender.send(record);return;
        }
        let record=match event_session(){Some(session)=>crate::connection_handler::rpc_session_event_record(&session,event,&event_abort_source),None=>crate::session_binding::session_event_record(event)};
        let _=sender.send(record);
    }));
    // The handler binds the RPC extension context, including `extension_error` records on the
    // same ordered stream as events and responses (senpi connection-handler.ts:937-945).
    let sink:crate::connection_handler::RpcRecordSink=std::sync::Arc::new(move|record|{let _=events_tx.send(Ok(record));});
    let mut handler=crate::connection_handler::RpcConnectionHandler::create(session.clone(),sink,capabilities).await;
    handler.last_abort_source=last_abort_source;
    let result=pump(&handler,input,output,&mut events,signal).await;
    drop(subscription);
    (handler,result)
}
/// The client capability flags a stdio host reads from `SENPI_RPC_CLIENT_CAPABILITIES` (senpi
/// `parseClientCapabilities(envValue("RPC_CLIENT_CAPABILITIES"))`).
pub fn env_client_capabilities()->Vec<String>{
    crate::custom_capability::parse_client_capabilities(maho_core::brand::env_value("RPC_CLIENT_CAPABILITIES",&maho_core::config::current_env()).as_deref())
}
pub async fn run_command_stream(session:&maho_core::agent_session::AgentSession,input:impl tokio::io::AsyncRead+Unpin,mut output:impl tokio::io::AsyncWrite+Unpin)->std::io::Result<()>{
    use tokio::io::AsyncWriteExt;
    let(handler,result)=serve(session,env_client_capabilities(),input,&mut output,&mut std::future::pending::<i32>()).await;
    // The stream ends this connection only; the session outlives it.
    handler.release().await;
    match result{Ok(_)=>output.flush().await,Err(error)=>Err(error)}
}
/// SIGTERM ends with 143 and SIGHUP with 129 (senpi `registerSignalHandlers`).
#[cfg(unix)]
async fn termination_signal()->i32{
    use tokio::signal::unix::{signal,SignalKind};
    let (Ok(mut term),Ok(mut hup))=(signal(SignalKind::terminate()),signal(SignalKind::hangup())) else{return std::future::pending().await;};
    tokio::select!{_=term.recv()=>143,_=hup.recv()=>129}
}
#[cfg(not(unix))]
async fn termination_signal()->i32{
    match tokio::signal::ctrl_c().await{Ok(())=>143,Err(_)=>std::future::pending().await}
}
/// Run RPC mode over supplied streams: the transport-independent process entry
/// (senpi `runRpcMode`). EOF, an extension's `ctx.shutdown()` and SIGTERM/SIGHUP share one
/// shutdown that releases the connection and yields the exit code; disposing the session and
/// exiting the process belong to the host.
pub async fn run_rpc_mode(session:&maho_core::agent_session::AgentSession,input:impl tokio::io::AsyncRead+Unpin,mut output:impl tokio::io::AsyncWrite+Unpin)->std::io::Result<i32>{
    use tokio::io::AsyncWriteExt;
    let mut signal=Box::pin(termination_signal());
    let(handler,result)=serve(session,env_client_capabilities(),input,&mut output,&mut signal).await;
    let code=match result{Ok(code)=>code,Err(error)=>{handler.release().await;return Err(error);}};
    let code=crate::shutdown::create_rpc_shutdown().shutdown(code,||handler.release()).await;
    // senpi skips the stdout flush on SIGTERM only.
    if code!=143{output.flush().await?;}
    Ok(code)
}
#[cfg(test)]
mod drain_tests {
    use super::*;
    use std::{pin::Pin,task::{Context,Poll}};
    struct ReplenishingWriter {
        sender:tokio::sync::mpsc::UnboundedSender<Result<serde_json::Value,serde_json::Error>>,
        records:Vec<serde_json::Value>,
        partial:Vec<u8>,
    }
    impl tokio::io::AsyncWrite for ReplenishingWriter {
        fn poll_write(mut self:Pin<&mut Self>,_:&mut Context<'_>,bytes:&[u8])->Poll<std::io::Result<usize>> {
            if self.records.len()>=64{return Poll::Ready(Err(std::io::Error::other("event drain exceeded its finite prefix")));}
            for byte in bytes {
                if *byte==b'\n' {
                    let record:serde_json::Value=serde_json::from_slice(&self.partial).unwrap();
                    self.partial.clear();
                    if record["type"]!="response" {self.sender.send(Ok(serde_json::json!({"type":"agent_settled"}))).unwrap();}
                    self.records.push(record);
                } else {self.partial.push(*byte);}
            }
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self:Pin<&mut Self>,_:&mut Context<'_>)->Poll<std::io::Result<()>>{Poll::Ready(Ok(()))}
        fn poll_shutdown(self:Pin<&mut Self>,_:&mut Context<'_>)->Poll<std::io::Result<()>>{Poll::Ready(Ok(()))}
    }
    #[tokio::test]
    async fn response_completes_when_each_event_write_synchronously_replenishes_queue() {
        let(sender,mut events)=tokio::sync::mpsc::unbounded_channel();
        sender.send(Ok(serde_json::json!({"type":"agent_settled"}))).unwrap();
        let mut writer=ReplenishingWriter{sender,records:Vec::new(),partial:Vec::new()};
        let response=crate::jsonl::serialize_json_line(&serde_json::json!({"type":"response","id":"req_1","success":true})).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2),finish_command(&mut writer,&mut events,Ok(Some(response)))).await.unwrap().unwrap();
        assert_eq!(writer.records.len(),2);
        assert_eq!(writer.records[0]["type"],"agent_settled");
        assert_eq!(writer.records[1]["id"],"req_1");
        assert_eq!(events.len(),1);
    }
}
#[cfg(test)]mod tests{use super::*;#[test]fn empty_chunks_do_not_emit_and_records_remain_individual_jsonl(){assert_eq!(linearize_output("\n\n").unwrap(),"");assert_eq!(linearize_output("{\"type\":\"agent_start\"}\n\n{\"type\":\"agent_idle\"}\n").unwrap(),"{\"type\":\"agent_start\"}\n{\"type\":\"agent_idle\"}\n");}#[test]fn stdio_message_updates_remove_snapshots(){let line=serde_json::json!({"type":"message_update","message":{"role":"assistant","usage":{}},"assistantMessageEvent":{"type":"text_delta","delta":"a","partial":{}}}).to_string();let output=linearize_output(&line).unwrap();let value:serde_json::Value=serde_json::from_str(&output).unwrap();assert!(value.get("message").is_none());assert_eq!(value["assistantMessageEvent"]["delta"],"a");}}
