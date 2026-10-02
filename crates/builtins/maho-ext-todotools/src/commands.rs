use crate::{todo_types::{TodoPhase,TodoStatus,TodoItem,DEFAULT_INIT_PHASE},markdown::phases_to_markdown};
use crate::{todo_types::{TodoOperation,TodoOpEntry},todo_operations::apply_ops_to_phases};
pub struct TodoCommandMutation { pub phases:Vec<TodoPhase>,pub action:String,pub notification:String,pub removed:bool }
pub async fn export_to_file(phases:&[TodoPhase],rest:&str,cwd:&std::path::Path)->Result<Option<std::path::PathBuf>,String> {
    if phases.is_empty() { return Ok(None); }
    let target=crate::markdown::resolve_todo_markdown_path(rest,cwd);
    tokio::fs::write(&target,phases_to_markdown(phases)).await.map_err(|error|format!("Failed to write todos: {error}"))?;
    Ok(Some(target))
}
pub async fn import_from_file(rest:&str,cwd:&std::path::Path)->Result<TodoCommandMutation,String> {
    let source=crate::markdown::resolve_todo_markdown_path(rest,cwd);
    let bytes=tokio::fs::read(&source).await.map_err(|error|format!("Failed to read todos: {error}"))?;
    let parsed=crate::markdown::markdown_to_phases(&String::from_utf8_lossy(&bytes));
    if !parsed.errors.is_empty() { return Err(format!("Could not parse {}:\n  {}",source.display(),parsed.errors.join("\n  "))); }
    let tasks=parsed.phases.iter().map(|phase|phase.tasks.len()).sum::<usize>();
    Ok(TodoCommandMutation{action:format!("/todo import {}",source.display()),notification:format!("Imported {} phase(s), {tasks} task(s) from {}.",parsed.phases.len(),source.display()),phases:parsed.phases,removed:false})
}
fn js_whitespace(c:char)->bool { matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
pub fn argument_completions(prefix:&str)->Option<Vec<&'static str>> {
    let prefix=prefix.trim_matches(js_whitespace).to_lowercase();
    let matches:Vec<_>=["edit","copy","export","import","append","start","done","drop","rm","help"].into_iter().filter(|verb|verb.starts_with(&prefix)).collect();
    (!matches.is_empty()).then_some(matches)
}
pub fn split_command(args:&str)->Option<(String,&str)> {
    let trimmed=args.trim_matches(js_whitespace); if trimmed.is_empty() { return None; }
    match trimmed.char_indices().find(|(_,character)|js_whitespace(*character)) {
        Some((index,character))=>Some((trimmed[..index].to_lowercase(),trimmed[index+character.len_utf8()..].trim_matches(js_whitespace))),
        None=>Some((trimmed.to_lowercase(),"")),
    }
}
pub fn status_command(phases:&[TodoPhase],rest:&str,op:TodoOperation)->Result<TodoCommandMutation,String> {
    if op==TodoOperation::Start {
        if rest.is_empty() { return Err("Usage: /todo start <task>".into()); }
        if find_task_fuzzy(phases,rest).is_none() { return Err(format!("No task matched \"{rest}\". Use /todo to list current tasks.")); }
    }
    let trimmed=rest.trim_matches(js_whitespace);
    let (task,phase,label)=if trimmed.is_empty() { (None,None,None) }
    else if let Some((p,t))=find_task_fuzzy(phases,trimmed) { (Some(phases[p].tasks[t].content.clone()),None,Some(phases[p].tasks[t].content.clone())) }
    else if op!=TodoOperation::Start { match find_phase_fuzzy(phases,trimmed) { Some(phase)=>(None,Some(phase.name.clone()),Some(phase.name.clone())),None=>return Err(format!("No task or phase matched \"{trimmed}\".")) } }
    else { return Err(format!("No task matched \"{rest}\". Use /todo to list current tasks.")); };
    if op==TodoOperation::Rm && trimmed.is_empty() { return Ok(TodoCommandMutation{phases:vec![],action:"/todo rm (all)".into(),notification:"Cleared all todos.".into(),removed:true}); }
    let is_phase=phase.is_some();
    let result=apply_ops_to_phases(phases,&[TodoOpEntry{op,list:None,task,phase,items:None}]);
    if !result.errors.is_empty() { return Err(result.errors.join("; ")); }
    let verb=match op { TodoOperation::Start=>"start",TodoOperation::Done=>"done",TodoOperation::Drop=>"drop",TodoOperation::Rm=>"rm",_=>unreachable!("status command accepts start/done/drop/rm") };
    let target=if op==TodoOperation::Done { "completed" } else { "abandoned" };
    let notification=match (op,label.as_deref(),is_phase) {
        (TodoOperation::Start,Some(label),_)=>format!("Started: {label}"),
        (TodoOperation::Rm,Some(label),true)=>format!("Removed phase: {label}"),
        (TodoOperation::Rm,Some(label),false)=>format!("Removed: {label}"),
        (_,Some(label),true)=>format!("Marked phase {label} {target}."),
        (_,Some(label),false)=>format!("Marked {target}: {label}"),
        _=>format!("Marked all tasks {target}."),
    };
    Ok(TodoCommandMutation{phases:result.phases,action:format!("/todo {verb} {}",label.as_deref().unwrap_or("(all)")),notification,removed:op==TodoOperation::Rm})
}
fn title_case_sentence(text:&str)->String {
    let text=text.trim_matches(js_whitespace); let mut chars=text.chars(); match chars.next() { Some(first) if first.len_utf16()==2=>text.into(),Some(first)=>format!("{}{}",first.to_uppercase(),chars.as_str()),None=>String::new() }
}
pub fn append_command(phases:&[TodoPhase],rest:&str)->Result<(Vec<TodoPhase>,String,String),String> {
    let tokens=tokenize_todo_args(rest); if tokens.is_empty() { return Err("Usage: /todo append [<phase>] <task...>".into()); }
    let mut next=phases.to_vec();
    let (phase_name,content)=if tokens.len()==1 { (None,tokens[0].clone()) } else { (Some(tokens[0].as_str()),tokens[1..].join(" ")) };
    let index=if let Some(name)=phase_name {
        if let Some(found)=find_phase_fuzzy(&next,name) { next.iter().position(|phase|std::ptr::eq(phase,found)).expect("phase belongs to list") }
        else { next.push(TodoPhase{name:name.split(js_whitespace).filter(|word|!word.is_empty()).map(title_case_sentence).collect::<Vec<_>>().join(" "),tasks:vec![]}); next.len()-1 }
    } else if !next.is_empty() { next.len()-1 } else { next.push(TodoPhase{name:DEFAULT_INIT_PHASE.into(),tasks:vec![]}); 0 };
    let content=title_case_sentence(&content); next[index].tasks.push(TodoItem{content:content.clone(),status:TodoStatus::Pending});
    let action=format!("/todo append → {}",next[index].name); let notification=format!("Appended to {}: {content}",next[index].name); Ok((next,action,notification))
}
pub fn build_user_edit_reminder(action:&str,phases:&[TodoPhase],removed:bool)->String {
    let markdown=if phases.is_empty() { "(empty)".into() } else { phases_to_markdown(phases).trim_end().to_owned() };
    let mut lines=vec!["<system-reminder>".into(),format!("The user manually modified the todo list ({action}).")];
    if removed { lines.push(if phases.is_empty() { "The user intentionally cleared the todo list. Do NOT recreate or re-populate it unless the user explicitly asks; continue the current request without a todo list." } else { "The user intentionally removed the entries no longer shown below. Do NOT re-add them unless the user explicitly asks." }.into()); }
    lines.extend(["Current todo list:".into(),String::new(),markdown,"</system-reminder>".into()]); lines.join("\n")
}
pub fn tokenize_todo_args(input:&str)->Vec<String> {
    let mut tokens=Vec::new(); let mut current=String::new(); let mut quoted=false; let mut chars=input.chars();
    while let Some(c)=chars.next() {
        if c=='\\' && let Some(next)=chars.next() { current.push(next); continue; }
        if c=='"' { quoted = !quoted; continue; }
        if !quoted && matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') { if !current.is_empty() { tokens.push(std::mem::take(&mut current)); } continue; }
        current.push(c);
    }
    if !current.is_empty() { tokens.push(current); } tokens
}
pub fn find_phase_fuzzy<'a>(phases:&'a [TodoPhase],query:&str)->Option<&'a TodoPhase> {
    let query=query.trim_matches(js_whitespace).to_lowercase(); if query.is_empty() { return None; }
    if let Some(phase)=phases.iter().find(|phase|phase.name.to_lowercase()==query) { return Some(phase); }
    let mut prefix=phases.iter().filter(|phase|phase.name.to_lowercase().starts_with(&query)); let first=prefix.next(); if first.is_some() && prefix.next().is_none() { return first; }
    let mut substring=phases.iter().filter(|phase|phase.name.to_lowercase().contains(&query)); let first=substring.next(); if substring.next().is_none() { first } else { None }
}
pub fn find_task_fuzzy(phases:&[TodoPhase],query:&str)->Option<(usize,usize)> {
    let query=query.trim_matches(js_whitespace).to_lowercase(); if query.is_empty() { return None; }
    let mut matches=Vec::new();
    for (phase_index,phase) in phases.iter().enumerate() { for (task_index,task) in phase.tasks.iter().enumerate() { let content=task.content.to_lowercase(); if content==query { return Some((phase_index,task_index)); } else if content.contains(&query) { matches.push((phase_index,task_index)); } } }
    if matches.len()==1 { return matches.first().copied(); }
    let mut open=matches.into_iter().filter(|(phase,task)|matches!(phases[*phase].tasks[*task].status,TodoStatus::Pending|TodoStatus::InProgress)); let first=open.next(); if open.next().is_none() { first } else { None }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn markdown_file_commands_roundtrip_real_file_and_skip_empty_export() {
        let directory=tempfile::tempdir().unwrap(); assert!(export_to_file(&[],"",directory.path()).await.unwrap().is_none()); assert!(!directory.path().join("TODO.md").exists());
        let phases=vec![TodoPhase{name:"Build".into(),tasks:vec![TodoItem{content:"Run tests".into(),status:TodoStatus::InProgress}]}];
        assert_eq!(export_to_file(&phases,"",directory.path()).await.unwrap().unwrap(),directory.path().join("TODO.md"));
        let imported=import_from_file("",directory.path()).await.unwrap(); assert_eq!(imported.phases,phases); assert!(!imported.removed);
    }
    #[tokio::test] async fn malformed_import_does_not_return_mutated_phases() {
        let directory=tempfile::tempdir().unwrap(); std::fs::write(directory.path().join("TODO.md"),"not a checklist").unwrap(); assert!(import_from_file("",directory.path()).await.is_err());
        assert!(import_from_file("missing.md",directory.path()).await.is_err()); assert!(export_to_file(&[TodoPhase{name:"Build".into(),tasks:vec![]}],"missing/TODO.md",directory.path()).await.is_err());
    }
    #[test] fn command_dispatch_uses_javascript_whitespace_and_preserves_arguments() { assert_eq!(split_command(" \u{feff}APPEND\u{feff}\"two words\" "),Some(("append".into(),"\"two words\""))); assert_eq!(split_command("\u{feff}"),None); assert_eq!(split_command("help"),Some(("help".into(),""))); }
    #[test] fn completion_preserves_source_order_and_no_match_is_absent() { assert_eq!(argument_completions("\u{feff}D"),Some(vec!["done","drop"])); assert_eq!(argument_completions("missing"),None); assert_eq!(argument_completions("").unwrap().len(),10); }
    #[test] fn whitespace_start_is_an_unmatched_target_not_an_untargeted_operation() { let phases=vec![TodoPhase{name:"Build".into(),tasks:vec![TodoItem{content:"Run tests".into(),status:TodoStatus::Pending}]}]; assert!(status_command(&phases," \u{feff}",TodoOperation::Start).is_err()); assert_eq!(phases[0].tasks[0].status,TodoStatus::Pending); }
    #[test] fn sentence_title_case_preserves_astral_first_code_unit() { assert_eq!(title_case_sentence("\u{10428}task"),"\u{10428}task"); assert_eq!(title_case_sentence("\u{feff}task\u{feff}"),"Task"); }
    #[test] fn tokenizer_uses_javascript_whitespace() { assert_eq!(tokenize_todo_args("a\u{feff}b\u{0085}c"),["a","b\u{0085}c"]); }
    #[test] fn quoted_and_escaped_tokens() { assert_eq!(tokenize_todo_args("append \"two words\" escaped\\ space end\\"),["append","two words","escaped space","end\\"]); }
    #[test] fn empty_quotes_do_not_create_tokens() { assert_eq!(tokenize_todo_args("\"\" a"),["a"]); }
    #[test] fn removing_all_does_not_leave_empty_phase() { let phases=vec![TodoPhase{name:"Build".into(),tasks:vec![]}]; let mutation=status_command(&phases,"",TodoOperation::Rm).unwrap(); assert!(mutation.phases.is_empty()); assert!(mutation.removed); }
    #[test] fn status_fuzzy_task_completion_promotes_next() { let phases=vec![TodoPhase{name:"Build".into(),tasks:vec![TodoItem{content:"Write parser".into(),status:TodoStatus::InProgress},TodoItem{content:"Run tests".into(),status:TodoStatus::Pending}]}]; let mutation=status_command(&phases,"parser",TodoOperation::Done).unwrap(); assert_eq!(mutation.phases[0].tasks[0].status,TodoStatus::Completed); assert_eq!(mutation.phases[0].tasks[1].status,TodoStatus::InProgress); }
    #[test] fn append_uses_last_phase_and_keeps_duplicate_pending_tasks() { let phases=vec![TodoPhase{name:"Build".into(),tasks:vec![]}]; let (next,_,_)=append_command(&phases,"\"write tests\"").unwrap(); let (next,_,_)=append_command(&next,"\"write tests\"").unwrap(); assert_eq!(next[0].tasks.len(),2); assert_eq!(next[0].tasks[0].content,"Write tests"); assert_eq!(next[0].tasks[0].status,TodoStatus::Pending); assert!(phases[0].tasks.is_empty()); }
    #[test] fn append_creates_title_case_phase_and_rejects_empty_arguments() { let (next,_,_)=append_command(&[],"\"final checks\" run tests").unwrap(); assert_eq!(next[0].name,"Final Checks"); assert_eq!(next[0].tasks[0].content,"Run tests"); assert!(append_command(&[],"").is_err()); }
    #[test] fn phase_exact_and_unique_substring() { let phases=vec![TodoPhase{name:"Build app".into(),tasks:vec![]},TodoPhase{name:"Build tests".into(),tasks:vec![]}]; assert!(find_phase_fuzzy(&phases,"build").is_none()); assert_eq!(find_phase_fuzzy(&phases,"APP").unwrap().name,"Build app"); }
}
