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
pub async fn run_command_stream(session:&maho_core::agent_session::AgentSession,mut input:impl tokio::io::AsyncRead+Unpin,mut output:impl tokio::io::AsyncWrite+Unpin)->std::io::Result<()>{
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    let mut reader=crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS).expect("positive line limit");
    let mut bytes=[0;8192];
    let(events_tx,mut events)=tokio::sync::mpsc::unbounded_channel();
    let _subscription=session.subscribe(std::sync::Arc::new(move|event|{let _=events_tx.send(crate::session_binding::session_event_record(event));}));
    loop{
        let count=tokio::select!{
            event=events.recv()=>{
                if let Some(event)=event{
                    let event=event.map_err(std::io::Error::other)?;
                    let wire=crate::json_event::to_json_event(&event).map_err(std::io::Error::other)?;
                    output.write_all(crate::jsonl::serialize_json_line(&wire)?.as_bytes()).await?;
                }
                continue;
            },
            count=input.read(&mut bytes)=>count?,
        };
        let records=if count==0{reader.finish()}else{reader.push(&bytes[..count])};
        for record in records{
            let crate::jsonl::LineRecord::Line(line)=record else{
                let response=crate::jsonl::serialize_json_line(&serde_json::json!({"type":"response","command":"parse","success":false,"error":format!("RPC input line exceeds {} characters.",crate::jsonl::MAX_RPC_LINE_CHARACTERS)}))?;
                output.write_all(response.as_bytes()).await?;
                continue;
            };
            let response=crate::connection_handler::handle_input_line(session,&line).await?;
            let Some(response)=response else{return Err(std::io::Error::new(std::io::ErrorKind::Unsupported,"RPC command requires unfinished runtime binding"));};
            while let Ok(event)=events.try_recv(){
                let event=event.map_err(std::io::Error::other)?;
                let wire=crate::json_event::to_json_event(&event).map_err(std::io::Error::other)?;
                output.write_all(crate::jsonl::serialize_json_line(&wire)?.as_bytes()).await?;
            }
            output.write_all(response.as_bytes()).await?;
        }
        while let Ok(event)=events.try_recv(){
            let event=event.map_err(std::io::Error::other)?;
            let wire=crate::json_event::to_json_event(&event).map_err(std::io::Error::other)?;
            output.write_all(crate::jsonl::serialize_json_line(&wire)?.as_bytes()).await?;
        }
        if count==0{output.flush().await?;return Ok(());}
    }
}
#[cfg(test)]mod tests{use super::*;#[test]fn empty_chunks_do_not_emit_and_records_remain_individual_jsonl(){assert_eq!(linearize_output("\n\n").unwrap(),"");assert_eq!(linearize_output("{\"type\":\"agent_start\"}\n\n{\"type\":\"agent_idle\"}\n").unwrap(),"{\"type\":\"agent_start\"}\n{\"type\":\"agent_idle\"}\n");}#[test]fn stdio_message_updates_remove_snapshots(){let line=serde_json::json!({"type":"message_update","message":{"role":"assistant","usage":{}},"assistantMessageEvent":{"type":"text_delta","delta":"a","partial":{}}}).to_string();let output=linearize_output(&line).unwrap();let value:serde_json::Value=serde_json::from_str(&output).unwrap();assert!(value.get("message").is_none());assert_eq!(value["assistantMessageEvent"]["delta"],"a");}}
