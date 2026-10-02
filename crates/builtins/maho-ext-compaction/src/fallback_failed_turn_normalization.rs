use maho_ai::types::Message;

pub fn mark_failed_turn_fragments(messages: &[Message]) -> Vec<bool> {
    let retained = maho_ai::utils::drop_failed_assistant_turns::drop_failed_assistant_turns(messages);
    let mut next = 0;
    messages.iter().map(|message| {
        if retained.get(next) == Some(message) {
            next += 1;
            false
        } else {
            true
        }
    }).collect()
}
