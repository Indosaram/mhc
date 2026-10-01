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
#[cfg(test)]mod tests{use super::*;#[test]fn empty_chunks_do_not_emit_and_records_remain_individual_jsonl(){assert_eq!(linearize_output("\n\n").unwrap(),"");assert_eq!(linearize_output("{\"type\":\"agent_start\"}\n\n{\"type\":\"agent_idle\"}\n").unwrap(),"{\"type\":\"agent_start\"}\n{\"type\":\"agent_idle\"}\n");}#[test]fn stdio_message_updates_remove_snapshots(){let line=serde_json::json!({"type":"message_update","message":{"role":"assistant","usage":{}},"assistantMessageEvent":{"type":"text_delta","delta":"a","partial":{}}}).to_string();let output=linearize_output(&line).unwrap();let value:serde_json::Value=serde_json::from_str(&output).unwrap();assert!(value.get("message").is_none());assert_eq!(value["assistantMessageEvent"]["delta"],"a");}}
