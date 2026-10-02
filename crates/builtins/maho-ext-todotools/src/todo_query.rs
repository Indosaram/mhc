// Copyright (c) 2025 Mario Zechner; Copyright (c) 2025-2026 Can Bölük.
// Adapted from oh-my-pi's MIT-licensed todo tool via senpi.
use std::collections::BTreeMap;
use crate::todo_types::{TodoCompletionTransition,TodoItem,TodoPhase,TodoStatus};
pub fn find_task_by_content(phases:&[TodoPhase],content:&str)->Option<(usize,usize)> { phases.iter().enumerate().find_map(|(p,phase)|phase.tasks.iter().position(|t|t.content==content).map(|t|(p,t))) }
pub fn find_phase_by_name(phases:&[TodoPhase],name:&str)->Option<usize> { phases.iter().position(|p|p.name==name) }
pub fn normalize_in_progress_task(phases:&mut [TodoPhase]) {
    let mut found=false;
    for task in phases.iter_mut().flat_map(|p|&mut p.tasks) {
        if task.status==TodoStatus::InProgress { if found { task.status=TodoStatus::Pending; } else { found=true; } }
    }
    if found { return; }
    if let Some(task)=phases.iter_mut().flat_map(|p|&mut p.tasks).find(|t|t.status==TodoStatus::Pending) { task.status=TodoStatus::InProgress; }
}
pub fn next_actionable_task(phases:&[TodoPhase])->Option<&TodoItem> {
    let mut pending=None;
    for task in phases.iter().flat_map(|p|&p.tasks) {
        if task.status==TodoStatus::InProgress { return Some(task); }
        if pending.is_none() && task.status==TodoStatus::Pending { pending=Some(task); }
    }
    pending
}
pub fn get_completion_transitions(previous:&[TodoPhase],updated:&[TodoPhase])->Vec<TodoCompletionTransition> {
    let mut statuses=BTreeMap::new();
    for phase in previous { for task in &phase.tasks { statuses.insert(format!("{}\0{}",phase.name,task.content),task.status); } }
    let mut transitions=Vec::new();
    for phase in updated { for task in &phase.tasks {
        if task.status==TodoStatus::Completed && statuses.get(&format!("{}\0{}",phase.name,task.content)).is_some_and(|s|*s!=TodoStatus::Completed) { transitions.push(TodoCompletionTransition{phase:phase.name.clone(),content:task.content.clone()}); }
    } }
    transitions
}
#[cfg(test)]
mod tests {
    use super::*;
    fn phases(statuses:&[TodoStatus])->Vec<TodoPhase> { vec![TodoPhase{name:"Setup".into(),tasks:statuses.iter().enumerate().map(|(i,s)|TodoItem{content:format!("task {i}"),status:*s}).collect()}] }
    #[test] fn exact_lookup() { let p=phases(&[TodoStatus::Pending]); assert_eq!(find_task_by_content(&p,"task 0"),Some((0,0))); assert_eq!(find_task_by_content(&p,"task-1"),None); assert_eq!(find_phase_by_name(&p,"setup"),None); }
    #[test] fn promotes_first_pending() { let mut p=phases(&[TodoStatus::Completed,TodoStatus::Pending]); normalize_in_progress_task(&mut p); assert_eq!(p[0].tasks[1].status,TodoStatus::InProgress); }
    #[test] fn keeps_first_active_only() { let mut p=phases(&[TodoStatus::InProgress,TodoStatus::InProgress]); normalize_in_progress_task(&mut p); assert_eq!(p[0].tasks[1].status,TodoStatus::Pending); }
    #[test] fn prefers_active_over_pending() { let p=phases(&[TodoStatus::Pending,TodoStatus::InProgress]); assert_eq!(next_actionable_task(&p).unwrap().content,"task 1"); }
    #[test] fn terminal_list_stays_terminal() { let mut p=phases(&[TodoStatus::Completed,TodoStatus::Abandoned]); normalize_in_progress_task(&mut p); assert!(next_actionable_task(&p).is_none()); }
    #[test] fn completion_only_for_existing_changed_tasks() { let before=phases(&[TodoStatus::Pending,TodoStatus::Completed]); let after=phases(&[TodoStatus::Completed,TodoStatus::Completed,TodoStatus::Completed]); assert_eq!(get_completion_transitions(&before,&after),vec![TodoCompletionTransition{phase:"Setup".into(),content:"task 0".into()}]); }
    #[test] fn empty_list() { let mut p=vec![]; normalize_in_progress_task(&mut p); assert!(next_actionable_task(&p).is_none()); }
}
