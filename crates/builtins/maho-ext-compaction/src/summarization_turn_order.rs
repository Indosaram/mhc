use maho_ai::types::{Message,StopReason};
pub fn normalize_summarization_turn_order(messages:&[Message])->Vec<Message> {
    let mut merged=Vec::new();
    for message in messages {
        if let Some(Message::Assistant(previous))=merged.last_mut()
            && let Message::Assistant(current)=message
            && !matches!(previous.stop_reason,StopReason::Error|StopReason::Aborted)
            && !matches!(current.stop_reason,StopReason::Error|StopReason::Aborted) {
            previous.content.extend(current.content.iter().cloned());continue;
        }
        merged.push(message.clone());
    }
    if let Some(index)=merged.iter().position(|message|matches!(message,Message::User(_)))
        && index!=merged.len().saturating_sub(1) {merged.drain(..index);}
    merged
}
