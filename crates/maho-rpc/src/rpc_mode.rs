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
async fn pump(session:&maho_core::agent_session::AgentSession,mut input:impl tokio::io::AsyncRead+Unpin,output:&mut(impl tokio::io::AsyncWrite+Unpin),events_tx:&tokio::sync::mpsc::UnboundedSender<Result<serde_json::Value,serde_json::Error>>,events:&mut tokio::sync::mpsc::UnboundedReceiver<Result<serde_json::Value,serde_json::Error>>)->std::io::Result<()>{
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
        if let Some((index,response))=completed{drop(commands.remove(index));finish_command(output,events,response).await?;continue;}
        // Dropping this owned future drops every pending command and the sole writer.
        if eof{finish_command(output,events,Ok(None)).await?;return Ok(());}
        let count=tokio::select!{
            event=events.recv()=>{let event=event.ok_or_else(||std::io::Error::other("RPC event channel closed while subscribed"))?;write_event(output,event).await?;continue;},
            completed=std::future::poll_fn(|cx|{
                for(index,command)in commands.iter_mut().enumerate(){if let Poll::Ready(response)=command.as_mut().poll(cx){return Poll::Ready((index,response));}}
                Poll::Pending
            }),if !commands.is_empty()=>{let(index,response)=completed;drop(commands.remove(index));finish_command(output,events,response).await?;continue;},
            count=input.read(&mut bytes)=>count?,
        };
        let records=if count==0{reader.finish()}else{reader.push(&bytes[..count])};
        for record in records{
            match record{
                crate::jsonl::LineRecord::Line(line)=>{
                    let sender=events_tx.clone();
                    commands.push(Box::pin(async move{
                        let sink:crate::connection_handler::RpcRecordSink=std::sync::Arc::new(move|record|{let _=sender.send(Ok(record));});
                        crate::connection_handler::handle_input_line_with_sink(session,&line,Some(sink)).await
                    }));
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
pub async fn run_command_stream(session:&maho_core::agent_session::AgentSession,input:impl tokio::io::AsyncRead+Unpin,mut output:impl tokio::io::AsyncWrite+Unpin)->std::io::Result<()>{
    use tokio::io::AsyncWriteExt;
    let(events_tx,mut events)=tokio::sync::mpsc::unbounded_channel();
    let sender=events_tx.clone();
    let subscription=session.subscribe(std::sync::Arc::new(move|event|{let _=sender.send(crate::session_binding::session_event_record(event));}));
    let result=pump(session,input,&mut output,&events_tx,&mut events).await;
    drop(subscription);
    match result{Ok(())=>output.flush().await,Err(error)=>Err(error)}
}
/// Run RPC mode over supplied streams: the transport-independent process entry
/// (senpi `runRpcMode`). Signal handling, stdin wiring and exit belong to the host.
pub async fn run_rpc_mode(session:&maho_core::agent_session::AgentSession,input:impl tokio::io::AsyncRead+Unpin,output:impl tokio::io::AsyncWrite+Unpin)->std::io::Result<i32>{run_command_stream(session,input,output).await?;Ok(0)}
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
