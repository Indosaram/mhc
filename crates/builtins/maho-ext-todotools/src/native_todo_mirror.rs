// Port of senpi todotools/native-todo-mirror.ts (MIT).
use serde_json::Value;
use crate::todo_types::{TodoItem, TodoPhase, TodoStatus};

pub fn phases_from_cursor_todos(todos: &Value) -> Option<Vec<TodoPhase>> {
    let items = todos.as_array()?;
    let tasks: Vec<TodoItem> = items.iter().filter_map(|item| {
        let content = item.get("content")?.as_str()?.trim();
        if content.is_empty() { return None; }
        let status = match item.get("status").and_then(Value::as_str) {
            Some("in_progress") => TodoStatus::InProgress,
            Some("completed") => TodoStatus::Completed,
            Some("abandoned") => TodoStatus::Abandoned,
            _ => TodoStatus::Pending,
        };
        Some(TodoItem { content: content.into(), status })
    }).collect();
    Some(if tasks.is_empty() { vec![] } else { vec![TodoPhase { name: "Tasks".into(), tasks }] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn absent_payload_differs_from_empty_list() {
        assert_eq!(phases_from_cursor_todos(&Value::Null), None);
        assert_eq!(phases_from_cursor_todos(&json!([])), Some(vec![]));
    }
    #[test]
    fn cursor_tasks_keep_status_and_drop_blank_items() {
        let input = json!([{"content":"build","status":"completed"},
            {"content":"link","status":"in_progress"}, {"content":"  ","status":"pending"}]);
        assert_eq!(phases_from_cursor_todos(&input), Some(vec![TodoPhase {
            name:"Tasks".into(), tasks:vec![
                TodoItem {content:"build".into(),status:TodoStatus::Completed},
                TodoItem {content:"link".into(),status:TodoStatus::InProgress},
            ],
        }]));
    }
    #[test]
    fn malformed_items_are_omitted_and_unknown_status_is_pending() {
        let input = json!([null, 1, {}, {"content":4}, {"content":" valid ","status":"unknown"}]);
        assert_eq!(phases_from_cursor_todos(&input).unwrap()[0].tasks,
            vec![TodoItem {content:"valid".into(),status:TodoStatus::Pending}]);
    }
}
