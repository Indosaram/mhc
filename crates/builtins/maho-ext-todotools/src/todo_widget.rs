use crate::{todo_types::*,todo_query::next_actionable_task,todo_format::{sanitize_todo_text,get_todo_marker}};
#[derive(Clone,Debug,PartialEq,Eq)]
pub enum TodoWidgetRow { Label(String),Task(TodoItem) }
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct TodoWidgetModel { pub phase_name:String,pub rows:Vec<TodoWidgetRow> }
fn omitted(count:usize,direction:&str)->TodoWidgetRow { TodoWidgetRow::Label(format!("... ({count} {direction} task{})",if count==1 { "" } else { "s" })) }
pub fn get_todo_widget_model(phases:&[TodoPhase])->Option<TodoWidgetModel> {
    let active=next_actionable_task(phases)?;
    let phase=phases.iter().find(|p|p.tasks.iter().any(|t|std::ptr::eq(t,active)))?;
    let mut rows=vec![TodoWidgetRow::Label("Todo".into()),TodoWidgetRow::Label(sanitize_todo_text(&phase.name))];
    if phase.tasks.len()+2<=10 { rows.extend(phase.tasks.iter().cloned().map(TodoWidgetRow::Task)); }
    else {
        let index=phase.tasks.iter().position(|t|std::ptr::eq(t,active))?;
        let first=index.saturating_sub(2);
        let recent=&phase.tasks[first..=index];
        let upcoming:Vec<_>=phase.tasks[index+1..].iter().filter(|t|t.status==TodoStatus::Pending).collect();
        let budget=8-recent.len()-usize::from(first>0);
        let visible=if upcoming.len()>budget { budget.saturating_sub(1) } else { upcoming.len() };
        if first>0 { rows.push(omitted(first,"earlier")); }
        rows.extend(recent.iter().cloned().map(TodoWidgetRow::Task));
        rows.extend(upcoming.iter().take(visible).map(|t|TodoWidgetRow::Task((*t).clone())));
        if upcoming.len()>visible { rows.push(omitted(upcoming.len()-visible,"later")); }
    }
    Some(TodoWidgetModel{phase_name:phase.name.clone(),rows})
}
pub fn get_todo_widget_lines(phases:&[TodoPhase])->Option<Vec<String>> {
    Some(get_todo_widget_model(phases)?.rows.into_iter().map(|row|match row {
        TodoWidgetRow::Label(text)=>text,
        TodoWidgetRow::Task(task)=> {
            let status=match task.status { TodoStatus::Pending=>"pending",TodoStatus::InProgress=>"in_progress",TodoStatus::Completed=>"completed",TodoStatus::Abandoned=>"abandoned" };
            format!("{} {}",get_todo_marker(status),sanitize_todo_text(&task.content))
        }
    }).collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn task(content:&str,status:TodoStatus)->TodoItem { TodoItem{content:content.into(),status} }
    fn phases(tasks:Vec<TodoItem>)->Vec<TodoPhase> { vec![TodoPhase{name:"Review".into(),tasks}] }
    #[test] fn active_first_and_last_stay_visible() {
        let first=phases(std::iter::once(task("Active",TodoStatus::InProgress)).chain((1..=9).map(|i|task(&format!("Pending {i}"),TodoStatus::Pending))).collect());
        let last=phases((1..=9).map(|i|task(&format!("Completed {i}"),TodoStatus::Completed)).chain(std::iter::once(task("Active",TodoStatus::InProgress))).collect());
        assert_eq!(get_todo_widget_lines(&first).unwrap(),vec!["Todo","Review","[•] Active","[ ] Pending 1","[ ] Pending 2","[ ] Pending 3","[ ] Pending 4","[ ] Pending 5","[ ] Pending 6","... (3 later tasks)"]);
        assert_eq!(get_todo_widget_lines(&last).unwrap(),vec!["Todo","Review","... (7 earlier tasks)","[✓] Completed 8","[✓] Completed 9","[•] Active"]);
    }
    #[test] fn exact_body_and_terminal_hide() {
        let p=phases((0..8).map(|i|task(&format!("Task {i}"),TodoStatus::Pending)).collect());
        assert_eq!(get_todo_widget_lines(&p).unwrap().len(),10);
        assert!(get_todo_widget_lines(&phases(vec![task("Closed",TodoStatus::Completed)])).is_none());
    }
    #[test] fn forward_window_counts_only_pending_tasks() {
        let p=phases(vec![task("before",TodoStatus::Completed),task("Active",TodoStatus::InProgress),task("done",TodoStatus::Completed),task("drop",TodoStatus::Abandoned)].into_iter().chain((1..=7).map(|i|task(&format!("Pending {i}"),TodoStatus::Pending))).collect());
        let lines=get_todo_widget_lines(&p).unwrap(); assert_eq!(lines.len(),10); assert_eq!(lines.last().unwrap(),"... (2 later tasks)"); assert!(!lines.iter().any(|line|line=="[×] drop"));
    }
    #[test] fn terminal_ahead_is_not_later_work() {
        let p=phases((1..=5).map(|i|task(&format!("Completed {i}"),TodoStatus::Completed)).chain([task("Active",TodoStatus::InProgress),task("ahead",TodoStatus::Completed),task("drop",TodoStatus::Abandoned),task("last",TodoStatus::Completed)]).collect());
        assert_eq!(get_todo_widget_lines(&p).unwrap().len(),6);
    }
    #[test] fn singular_earlier_omission() {
        let p=phases((1..=3).map(|i|task(&format!("Completed {i}"),TodoStatus::Completed)).chain(std::iter::once(task("Active",TodoStatus::InProgress))).chain((1..=6).map(|i|task(&format!("Pending {i}"),TodoStatus::Pending))).collect());
        assert_eq!(get_todo_widget_lines(&p).unwrap()[2],"... (1 earlier task)");
    }
}
