use crate::todo_types::{TodoPhase,TodoStatus};
pub fn tokenize_todo_args(input:&str)->Vec<String> {
    let mut tokens=Vec::new(); let mut current=String::new(); let mut quoted=false; let mut chars=input.chars();
    while let Some(c)=chars.next() {
        if c=='\\' && let Some(next)=chars.next() { current.push(next); continue; }
        if c=='"' { quoted = !quoted; continue; }
        if !quoted && c.is_whitespace() { if !current.is_empty() { tokens.push(std::mem::take(&mut current)); } continue; }
        current.push(c);
    }
    if !current.is_empty() { tokens.push(current); } tokens
}
pub fn find_phase_fuzzy<'a>(phases:&'a [TodoPhase],query:&str)->Option<&'a TodoPhase> {
    let query=query.trim().to_lowercase(); if query.is_empty() { return None; }
    if let Some(phase)=phases.iter().find(|phase|phase.name.to_lowercase()==query) { return Some(phase); }
    let mut prefix=phases.iter().filter(|phase|phase.name.to_lowercase().starts_with(&query)); let first=prefix.next(); if first.is_some() && prefix.next().is_none() { return first; }
    let mut substring=phases.iter().filter(|phase|phase.name.to_lowercase().contains(&query)); let first=substring.next(); if substring.next().is_none() { first } else { None }
}
pub fn find_task_fuzzy(phases:&[TodoPhase],query:&str)->Option<(usize,usize)> {
    let query=query.trim().to_lowercase(); if query.is_empty() { return None; }
    let mut matches=Vec::new();
    for (phase_index,phase) in phases.iter().enumerate() { for (task_index,task) in phase.tasks.iter().enumerate() { let content=task.content.to_lowercase(); if content==query { return Some((phase_index,task_index)); } else if content.contains(&query) { matches.push((phase_index,task_index)); } } }
    if matches.len()==1 { return matches.first().copied(); }
    let mut open=matches.into_iter().filter(|(phase,task)|matches!(phases[*phase].tasks[*task].status,TodoStatus::Pending|TodoStatus::InProgress)); let first=open.next(); if open.next().is_none() { first } else { None }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn quoted_and_escaped_tokens() { assert_eq!(tokenize_todo_args("append \"two words\" escaped\\ space end\\"),["append","two words","escaped space","end\\"]); }
    #[test] fn empty_quotes_do_not_create_tokens() { assert_eq!(tokenize_todo_args("\"\" a"),["a"]); }
    #[test] fn phase_exact_and_unique_substring() { let phases=vec![TodoPhase{name:"Build app".into(),tasks:vec![]},TodoPhase{name:"Build tests".into(),tasks:vec![]}]; assert!(find_phase_fuzzy(&phases,"build").is_none()); assert_eq!(find_phase_fuzzy(&phases,"APP").unwrap().name,"Build app"); }
}
