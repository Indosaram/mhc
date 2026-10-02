use std::collections::BTreeSet;
use crate::{todo_types::*,todo_query::{find_task_by_content,find_phase_by_name,next_actionable_task,normalize_in_progress_task},todo_resolution::{resolve_task_or_error,resolve_phase_or_error,get_task_targets}};
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct TodoApplyResult { pub phases:Vec<TodoPhase>,pub errors:Vec<String> }
pub fn init_phases(entry:&TodoOpEntry,errors:&mut Vec<String>,mut corrections:Option<&mut Vec<String>>)->Vec<TodoPhase> {
    let list=entry.list.clone().or_else(||entry.items.as_ref().filter(|items|!items.is_empty()).map(|items|vec![TodoPhaseInput{phase:entry.phase.clone().unwrap_or_else(||DEFAULT_INIT_PHASE.into()),items:items.clone()}]));
    let Some(list)=list else { errors.push("Missing list for init operation".into()); return vec![]; };
    let mut phases:Vec<TodoPhase>=vec![];
    let mut seen=BTreeSet::new();
    for item in list {
        if item.items.is_empty() { errors.push(format!("Phase \"{}\" has no tasks in init list",item.phase)); }
        let p=if let Some(p)=find_phase_by_name(&phases,&item.phase) {
            if let Some(c)=corrections.as_deref_mut() { c.push(format!("[auto-corrected] merged duplicate phase \"{}\" in init list",item.phase)); }
            p
        } else { phases.push(TodoPhase{name:item.phase,tasks:vec![]}); phases.len()-1 };
        for content in item.items {
            if !seen.insert(content.clone()) {
                if let Some(c)=corrections.as_deref_mut() { c.push(format!("[auto-corrected] kept first duplicate task \"{content}\" in init list")); }
                continue;
            }
            phases[p].tasks.push(TodoItem{content,status:TodoStatus::Pending});
        }
    }
    phases
}
pub fn append_items(phases:&mut Vec<TodoPhase>,entry:&TodoOpEntry,errors:&mut Vec<String>,corrections:Option<&mut Vec<String>>) {
    let Some(items)=entry.items.as_ref().filter(|items|!items.is_empty()) else { errors.push("Missing items for append operation".into()); return; };
    let mut seen=BTreeSet::new();
    let mut duplicate=false;
    for content in items { if !seen.insert(content) || find_task_by_content(phases,content).is_some() { errors.push(format!("Task \"{content}\" already exists")); duplicate=true; } }
    if duplicate { return; }
    let name=entry.phase.as_ref().filter(|s|!s.is_empty()).cloned().unwrap_or_else(|| {
        let name=next_actionable_task(phases).and_then(|task|phases.iter().find(|p|p.tasks.iter().any(|t|std::ptr::eq(t,task)))).or_else(||phases.last()).map(|p|p.name.clone()).unwrap_or_else(||DEFAULT_INIT_PHASE.into());
        if let Some(c)=corrections { c.push(format!("[auto-corrected] append had no phase; used \"{name}\"")); }
        name
    });
    let p=find_phase_by_name(phases,&name).unwrap_or_else(|| { phases.push(TodoPhase{name,tasks:vec![]}); phases.len()-1 });
    phases[p].tasks.extend(items.iter().map(|content|TodoItem{content:content.clone(),status:TodoStatus::Pending}));
}
pub fn remove_tasks(phases:&mut [TodoPhase],entry:&TodoOpEntry,errors:&mut Vec<String>,corrections:Option<&mut Vec<String>>) {
    if let Some(task)=entry.task.as_deref().filter(|s|!s.is_empty()) {
        if let Some((p,t))=resolve_task_or_error(phases,Some(task),errors,corrections) { phases[p].tasks.remove(t); }
    } else if let Some(phase)=entry.phase.as_deref().filter(|s|!s.is_empty()) {
        if let Some(p)=resolve_phase_or_error(phases,Some(phase),errors,corrections) { phases[p].tasks.clear(); }
    } else { for p in phases { p.tasks.clear(); } }
}
pub fn apply_entry(mut phases:Vec<TodoPhase>,entry:&TodoOpEntry,errors:&mut Vec<String>,corrections:Option<&mut Vec<String>>)->Vec<TodoPhase> {
    match entry.op {
        TodoOperation::Init => return init_phases(entry,errors,corrections),
        TodoOperation::Start => {
            if let Some((p,t))=resolve_task_or_error(&phases,entry.task.as_deref(),errors,corrections) {
                for task in phases.iter_mut().flat_map(|p|&mut p.tasks) { if task.status==TodoStatus::InProgress { task.status=TodoStatus::Pending; } }
                phases[p].tasks[t].status=TodoStatus::InProgress;
            }
        }
        TodoOperation::Done | TodoOperation::Drop => {
            for (p,t) in get_task_targets(&phases,entry,errors,corrections) { phases[p].tasks[t].status=if entry.op==TodoOperation::Done { TodoStatus::Completed } else { TodoStatus::Abandoned }; }
        }
        TodoOperation::Rm => remove_tasks(&mut phases,entry,errors,corrections),
        TodoOperation::Append => append_items(&mut phases,entry,errors,corrections),
        TodoOperation::View => {},
    }
    phases
}
pub fn apply_params(phases:Vec<TodoPhase>,params:&TodoOpEntry,corrections:Option<&mut Vec<String>>)->TodoApplyResult {
    if params.op==TodoOperation::View { return TodoApplyResult{phases,errors:vec![]}; }
    let original=phases.clone();
    let mut errors=vec![];
    let mut next=apply_entry(phases,params,&mut errors,corrections);
    if !errors.is_empty() { return TodoApplyResult{phases:original,errors}; }
    normalize_in_progress_task(&mut next);
    TodoApplyResult{phases:next,errors}
}
pub fn apply_ops_to_phases(current:&[TodoPhase],ops:&[TodoOpEntry])->TodoApplyResult {
    let mut next=current.to_vec();
    let mut errors=vec![];
    for op in ops { next=apply_entry(next,op,&mut errors,None); }
    if !errors.is_empty() { return TodoApplyResult{phases:current.to_vec(),errors}; }
    normalize_in_progress_task(&mut next);
    TodoApplyResult{phases:next,errors}
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entry(op:TodoOperation)->TodoOpEntry { TodoOpEntry{op,list:None,task:None,phase:None,items:None} }
    fn phase(name:&str,content:&str,status:TodoStatus)->TodoPhase { TodoPhase{name:name.into(),tasks:vec![TodoItem{content:content.into(),status}]} }
    #[test] fn duplicate_phases_merge_without_reordering() { let mut e=entry(TodoOperation::Init); e.list=Some(vec![TodoPhaseInput{phase:"One".into(),items:vec!["first".into()]},TodoPhaseInput{phase:"Two".into(),items:vec!["second".into()]},TodoPhaseInput{phase:"One".into(),items:vec!["third".into()]}]); let mut corrections=vec![]; let result=init_phases(&e,&mut vec![],Some(&mut corrections)); assert_eq!(result[0].tasks.iter().map(|t|t.content.as_str()).collect::<Vec<_>>(),vec!["first","third"]); assert_eq!(result[1].name,"Two"); assert_eq!(corrections.len(),1); }
    #[test] fn duplicate_init_task_keeps_first_phase() { let mut e=entry(TodoOperation::Init); e.list=Some(vec![TodoPhaseInput{phase:"One".into(),items:vec!["same".into()]},TodoPhaseInput{phase:"Two".into(),items:vec!["same".into(),"other".into()]}]); let result=init_phases(&e,&mut vec![],None); assert_eq!(result[0].tasks.len(),1); assert_eq!(result[1].tasks[0].content,"other"); }
    #[test] fn empty_init_phase_errors() { let mut e=entry(TodoOperation::Init); e.list=Some(vec![TodoPhaseInput{phase:"Empty".into(),items:vec![]}]); let mut errors=vec![]; init_phases(&e,&mut errors,None); assert_eq!(errors.len(),1); }
    #[test] fn append_defaults_to_active_phase() { let mut p=vec![phase("Active","now",TodoStatus::InProgress),phase("Later","later",TodoStatus::Pending)]; let mut e=entry(TodoOperation::Append); e.items=Some(vec!["new".into()]); append_items(&mut p,&e,&mut vec![],None); assert_eq!(p[0].tasks[1].content,"new"); assert_eq!(p[1].tasks.len(),1); }
    #[test] fn append_defaults_to_last_terminal_phase() { let mut p=vec![phase("One","done",TodoStatus::Completed),phase("Two","drop",TodoStatus::Abandoned)]; let mut e=entry(TodoOperation::Append); e.items=Some(vec!["new".into()]); append_items(&mut p,&e,&mut vec![],None); assert_eq!(p[1].tasks[1].content,"new"); }
    #[test] fn append_creates_default_phase() { let mut p=vec![]; let mut e=entry(TodoOperation::Append); e.items=Some(vec!["first".into()]); append_items(&mut p,&e,&mut vec![],None); assert_eq!(p[0].name,DEFAULT_INIT_PHASE); }
    #[test] fn duplicate_append_is_atomic() { let p=vec![phase("Tasks","old",TodoStatus::Pending)]; let mut e=entry(TodoOperation::Append); e.items=Some(vec!["new".into(),"old".into()]); let result=apply_params(p.clone(),&e,None); assert_eq!(result.phases,p); assert_eq!(result.errors.len(),1); }
    #[test] fn negated_sibling_cannot_be_completed() { let p=vec![phase("Tasks","Do not deploy API to production",TodoStatus::Pending)]; let mut e=entry(TodoOperation::Done); e.task=Some("Deploy API to production".into()); let result=apply_params(p.clone(),&e,Some(&mut vec![])); assert_eq!(result.phases,p); assert_eq!(result.errors.len(),1); }
    #[test] fn done_normalized_target_promotes_earliest_open_task() { let p=vec![phase("One","first",TodoStatus::Pending),phase("Two","Second",TodoStatus::InProgress)]; let mut e=entry(TodoOperation::Done); e.task=Some(" second ".into()); let result=apply_params(p,&e,Some(&mut vec![])); assert_eq!(result.phases[1].tasks[0].status,TodoStatus::Completed); assert_eq!(result.phases[0].tasks[0].status,TodoStatus::InProgress); }
    #[test] fn drop_normalized_target_is_abandoned() { let mut e=entry(TodoOperation::Drop); e.task=Some(" old ".into()); let result=apply_params(vec![phase("Tasks","Old",TodoStatus::Pending)],&e,Some(&mut vec![])); assert_eq!(result.phases[0].tasks[0].status,TodoStatus::Abandoned); }
    #[test] fn remove_normalized_task_keeps_phase() { let mut e=entry(TodoOperation::Rm); e.task=Some(" old ".into()); let result=apply_params(vec![phase("Tasks","Old",TodoStatus::Pending)],&e,Some(&mut vec![])); assert!(result.phases[0].tasks.is_empty()); }
    #[test] fn failed_batch_restores_original_state() { let p=vec![phase("Tasks","first",TodoStatus::Pending)]; let mut done=entry(TodoOperation::Done); done.task=Some("first".into()); let mut missing=entry(TodoOperation::Start); missing.task=Some("unknown".into()); let result=apply_ops_to_phases(&p,&[done,missing]); assert_eq!(result.phases,p); assert_eq!(result.errors.len(),1); }
}
