use crate::{fuzzy_match::{fuzzy_resolve_phase,fuzzy_resolve_task},todo_query::{find_phase_by_name,find_task_by_content},todo_types::{TodoOpEntry,TodoPhase}};
fn is_task_id(content:&str)->bool { content.strip_prefix("task-").is_some_and(|s| !s.is_empty() && s.bytes().all(|b|b.is_ascii_digit())) }
pub fn resolve_task_or_error(phases:&[TodoPhase],content:Option<&str>,errors:&mut Vec<String>,corrections:Option<&mut Vec<String>>)->Option<(usize,usize)> {
    let Some(content)=content.filter(|s|!s.is_empty()) else { errors.push("Missing task content".into()); return None; };
    let (hit,suggestion)=if let Some(corrections)=corrections {
        let resolution=fuzzy_resolve_task(phases,content);
        if let Some((p,t))=resolution.hit {
            if resolution.corrected { corrections.push(format!("[auto-matched] task \"{content}\" -> \"{}\" — pass the exact text from the previous todo result next time",phases[p].tasks[t].content)); }
            return Some((p,t));
        }
        let mut names:Vec<&str>=vec![];
        let mut count=0;
        for phase in phases { for task in &phase.tasks { if task.content==content { count+=1; if !names.contains(&phase.name.as_str()) { names.push(&phase.name); } } } }
        if count>1 {
            errors.push(format!("Task \"{content}\" is ambiguous: duplicate task text exists in phases {}. Use /todo edit to make the task text unique.",names.iter().map(|n|format!("\"{n}\"")).collect::<Vec<_>>().join(", ")));
            return None;
        }
        (None,resolution.suggestion)
    } else { (find_task_by_content(phases,content),None) };
    if hit.is_none() {
        if is_task_id(content) { errors.push(format!("Task \"{content}\" not found. Tasks are referenced by content, not by IDs — pass the task's full text from the previous result.")); }
        else {
            let hint=if phases.iter().all(|p|p.tasks.is_empty()) { " (todo list is empty — was it replaced or not yet created?)" } else { "" };
            let suggestion=suggestion.map(|s|format!(" Did you mean \"{s}\"?")).unwrap_or_default();
            errors.push(format!("Task \"{content}\" not found{hint}{suggestion}"));
        }
    }
    hit
}
pub fn resolve_phase_or_error(phases:&[TodoPhase],name:Option<&str>,errors:&mut Vec<String>,corrections:Option<&mut Vec<String>>)->Option<usize> {
    let Some(name)=name.filter(|s|!s.is_empty()) else { errors.push("Missing phase name".into()); return None; };
    let Some(corrections)=corrections else {
        let hit=find_phase_by_name(phases,name);
        if hit.is_none() { errors.push(format!("Phase \"{name}\" not found")); }
        return hit;
    };
    let resolution=fuzzy_resolve_phase(phases,name);
    if let Some(p)=resolution.hit {
        if resolution.corrected { corrections.push(format!("[auto-matched] phase \"{name}\" -> \"{}\" — pass the exact text from the previous todo result next time",phases[p].name)); }
        return Some(p);
    }
    if phases.iter().filter(|p|p.name==name).count()>1 { errors.push(format!("Phase \"{name}\" is ambiguous: duplicate phase names exist. Use /todo edit to make the phase name unique.")); }
    else { errors.push(format!("Phase \"{name}\" not found{}",resolution.suggestion.map(|s|format!(" Did you mean \"{s}\"?")).unwrap_or_default())); }
    None
}
pub fn get_task_targets(phases:&[TodoPhase],entry:&TodoOpEntry,errors:&mut Vec<String>,corrections:Option<&mut Vec<String>>)->Vec<(usize,usize)> {
    if let Some(task)=entry.task.as_deref().filter(|s|!s.is_empty()) { return resolve_task_or_error(phases,Some(task),errors,corrections).into_iter().collect(); }
    if let Some(name)=entry.phase.as_deref().filter(|s|!s.is_empty()) {
        return resolve_phase_or_error(phases,Some(name),errors,corrections).map(|p|(0..phases[p].tasks.len()).map(|t|(p,t)).collect()).unwrap_or_default();
    }
    phases.iter().enumerate().flat_map(|(p,phase)|(0..phase.tasks.len()).map(move |t|(p,t))).collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::todo_types::{TodoItem,TodoStatus};
    fn phase(name:&str)->TodoPhase { TodoPhase{name:name.into(),tasks:vec![TodoItem{content:"Duplicate".into(),status:TodoStatus::Pending}]} }
    #[test] fn legacy_duplicate_resolution_keeps_first_hit() { assert_eq!(resolve_task_or_error(&[phase("One"),phase("Two")],Some("Duplicate"),&mut vec![],None),Some((0,0))); }
    #[test] fn model_duplicate_resolution_does_not_pick_a_task() { let mut errors=vec![]; assert_eq!(resolve_task_or_error(&[phase("One"),phase("Two")],Some("Duplicate"),&mut errors,Some(&mut vec![])),None); assert_eq!(errors.len(),1); }
    #[test] fn missing_task_id_remains_an_error() { let mut errors=vec![]; assert_eq!(resolve_task_or_error(&[phase("One")],Some("task-12"),&mut errors,Some(&mut vec![])),None); assert_eq!(errors.len(),1); }
    #[test] fn missing_target_is_not_bulk_resolution() { let mut errors=vec![]; assert_eq!(resolve_task_or_error(&[],None,&mut errors,None),None); assert_eq!(errors.len(),1); }
    #[test] fn normalized_phase_resolves_with_correction() { let mut corrections=vec![]; assert_eq!(resolve_phase_or_error(&[phase("One")],Some(" one "),&mut vec![],Some(&mut corrections)),Some(0)); assert_eq!(corrections.len(),1); }
}
