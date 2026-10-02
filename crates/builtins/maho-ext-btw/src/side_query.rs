use maho_ai::{types::AssistantMessageEvent,utils::event_stream::AssistantMessageEventStream};
pub const DEFAULT_ESTABLISHMENT_TIMEOUT_MS:u64=30_000;
pub const SIDE_QUERY_INSTRUCTION:&str="The user is asking a side question about the conversation so far, outside the main task. Answer it directly and concisely from the context above. Do not continue any task, do not modify anything, and do not treat this as new work.";
pub async fn collect_reply(stream:&AssistantMessageEventStream,timeout_ms:u64,mut on_text_delta:impl FnMut(&str))->Result<String,String>{
    let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(timeout_ms);let mut established=false;let mut reply=String::new();
    loop{
        let event=if established{stream.next().await}else{tokio::time::timeout_at(deadline,stream.next()).await.map_err(|_|format!("/btw provider did not produce an event within {}s",(timeout_ms as f64/1000.0).round()))?}.map_err(|error|error.message)?;
        let Some(event)=event else{break};
        if !matches!(event,AssistantMessageEvent::Start{..}){established=true;}
        match event{
            AssistantMessageEvent::TextDelta{delta,..}=>{reply.push_str(&delta);on_text_delta(&delta);}
            AssistantMessageEvent::Done{..}=>break,
            AssistantMessageEvent::Error{error,..}=>return Err(error.error_message.filter(|message|!message.is_empty()).unwrap_or_else(||"Side query failed".into())),
            _=>{}
        }
    }
    Ok(reply)
}
