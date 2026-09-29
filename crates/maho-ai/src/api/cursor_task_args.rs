//! Port of senpi packages/ai/src/api/cursor-task-args.ts.
// ported by todo 12

use serde_json::Value;

pub fn is_usable_cursor_task_args(args: &Value) -> bool {
    let Value::Object(map) = args else {
        return false;
    };
    if map.get("category").is_some_and(is_truthy) {
        return true;
    }
    if map.get("subagent_type").is_some_and(is_truthy) {
        return true;
    }
    if let Some(Value::String(prompt)) = map.get("prompt") {
        if !prompt.trim().is_empty() {
            return true;
        }
    }
    if let Some(Value::Array(tasks)) = map.get("tasks") {
        if !tasks.is_empty() {
            return true;
        }
    }
    false
}

fn is_truthy(value: &Value) -> bool {
    !matches!(value, Value::Null | Value::Bool(false)) && value != &Value::String(String::new())
}

pub fn keep_usable_cursor_task_args(previous: Value, next: Value) -> Value {
    if is_usable_cursor_task_args(&next) {
        return next;
    }
    if is_usable_cursor_task_args(&previous) {
        return previous;
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn given_category_when_checked_then_usable() {
        assert!(is_usable_cursor_task_args(&json!({ "category": "quick" })));
    }

    #[test]
    fn given_subagent_type_when_checked_then_usable() {
        assert!(is_usable_cursor_task_args(&json!({ "subagent_type": "explore" })));
    }

    #[test]
    fn given_nonempty_prompt_when_checked_then_usable() {
        assert!(is_usable_cursor_task_args(&json!({ "prompt": "do the thing" })));
    }

    #[test]
    fn given_whitespace_only_prompt_when_checked_then_not_usable() {
        assert!(!is_usable_cursor_task_args(&json!({ "prompt": "   " })));
    }

    #[test]
    fn given_nonempty_tasks_array_when_checked_then_usable() {
        assert!(is_usable_cursor_task_args(&json!({ "tasks": [{ "prompt": "x" }] })));
    }

    #[test]
    fn given_empty_tasks_array_when_checked_then_not_usable() {
        assert!(!is_usable_cursor_task_args(&json!({ "tasks": [] })));
    }

    #[test]
    fn given_empty_object_when_checked_then_not_usable() {
        assert!(!is_usable_cursor_task_args(&json!({})));
    }

    #[test]
    fn given_non_object_when_checked_then_not_usable() {
        assert!(!is_usable_cursor_task_args(&json!("not an object")));
        assert!(!is_usable_cursor_task_args(&json!(["array"])));
        assert!(!is_usable_cursor_task_args(&Value::Null));
    }

    #[test]
    fn given_usable_next_when_kept_then_next_wins() {
        let previous = json!({ "prompt": "old" });
        let next = json!({ "prompt": "new" });
        assert_eq!(keep_usable_cursor_task_args(previous, next.clone()), next);
    }

    #[test]
    fn given_unusable_next_and_usable_previous_when_kept_then_previous_wins() {
        let previous = json!({ "prompt": "old" });
        let next = json!({});
        assert_eq!(keep_usable_cursor_task_args(previous.clone(), next), previous);
    }

    #[test]
    fn given_both_unusable_when_kept_then_next_returned() {
        let previous = json!({});
        let next = json!({});
        assert_eq!(keep_usable_cursor_task_args(previous, next.clone()), next);
    }
}
