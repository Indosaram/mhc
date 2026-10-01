//! Port of `components/todo-strike.ts`.
use serde_json::Value;

pub const TODO_STRIKE_HOLD_FRAMES: i64 = 2;
pub const TODO_STRIKE_REVEAL_FRAMES: i64 = 12;
pub const TODO_STRIKE_TOTAL_FRAMES: i64 = TODO_STRIKE_HOLD_FRAMES + TODO_STRIKE_REVEAL_FRAMES;
pub const TODO_STRIKE_FRAME_INTERVAL_MS: u64 = 65;

pub fn strike_reveal_count(text: &str, frame: Option<i64>) -> Option<i64> {
    let frame = frame?;
    if frame <= TODO_STRIKE_HOLD_FRAMES {
        return Some(0);
    }
    let length = i64::try_from(text.chars().count()).unwrap_or(i64::MAX);
    if length == 0 {
        return None;
    }
    Some(
        (length * (frame - TODO_STRIKE_HOLD_FRAMES).min(TODO_STRIKE_REVEAL_FRAMES) + TODO_STRIKE_REVEAL_FRAMES - 1)
            / TODO_STRIKE_REVEAL_FRAMES,
    )
}

pub fn partial_strikethrough(text: &str, visible_chars: i64, strike: impl Fn(&str) -> String) -> String {
    if visible_chars <= 0 {
        return text.to_owned();
    }
    let chars: Vec<char> = text.chars().collect();
    if visible_chars >= i64::try_from(chars.len()).unwrap_or(i64::MAX) {
        return strike(text);
    }
    let split = usize::try_from(visible_chars).unwrap_or(usize::MAX).min(chars.len());
    let head: String = chars[..split].iter().collect();
    let tail: String = chars[split..].iter().collect();
    format!("{}{}", strike(&head), tail)
}

pub fn has_completed_todo_tasks(details: &Value) -> bool {
    details
        .as_object()
        .and_then(|object| object.get("completedTasks"))
        .and_then(Value::as_array)
        .is_some_and(|tasks| !tasks.is_empty())
}
