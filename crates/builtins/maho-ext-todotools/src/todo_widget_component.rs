use std::collections::BTreeSet;
use crate::{todo_widget::{TodoWidgetModel,TodoWidgetRow},todo_types::{TodoStatus,TodoCompletionTransition}};
pub fn get_animated_completion_keys(model:&TodoWidgetModel,completed_tasks:&[TodoCompletionTransition])->BTreeSet<String> {
    let visible:BTreeSet<_>=model.rows.iter().filter_map(|row|match row { TodoWidgetRow::Task(task) if task.status==TodoStatus::Completed=>Some(task.content.as_str()),_=>None }).collect();
    completed_tasks.iter().filter(|transition|transition.phase==model.phase_name && visible.contains(transition.content.as_str())).map(|transition|transition.content.clone()).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn animation_targets_only_visible_completed_tasks_in_current_phase() { let model=TodoWidgetModel{phase_name:"Current".into(),rows:vec![TodoWidgetRow::Task(crate::todo_types::TodoItem{content:"visible".into(),status:TodoStatus::Completed}),TodoWidgetRow::Task(crate::todo_types::TodoItem{content:"pending".into(),status:TodoStatus::Pending})]}; let transitions=[("Current","visible"),("Other","visible"),("Current","hidden"),("Current","pending")].map(|(phase,content)|TodoCompletionTransition{phase:phase.into(),content:content.into()}); assert_eq!(get_animated_completion_keys(&model,&transitions),BTreeSet::from(["visible".into()])); }
}
