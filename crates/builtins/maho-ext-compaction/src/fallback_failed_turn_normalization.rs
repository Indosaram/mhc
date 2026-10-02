use maho_ai::{types::Message, utils::drop_failed_assistant_turns::drop_failed_assistant_turns};

pub fn mark_failed_turn_fragments(messages: &[Message]) -> Vec<bool> {
    let retained = drop_failed_assistant_turns(messages);
    let mut next = retained.iter().peekable();
    messages.iter().map(|message| {
        if next.peek().is_some_and(|kept| *kept == message) { next.next(); false } else { true }
    }).collect()
}
