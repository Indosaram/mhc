use std::path::{Component,Path,PathBuf};
use regex::Regex;
use std::sync::LazyLock;
use crate::{todo_types::*,todo_query::normalize_in_progress_task,todo_operations::TodoApplyResult};
pub const DEFAULT_TODO_MARKDOWN_FILE:&str="TODO.md";
pub fn resolve_todo_markdown_path(input:&str,cwd:&Path)->PathBuf {
    let raw=input.trim();
    let raw=raw.strip_prefix(['\'','"']).unwrap_or(raw);
    let raw=raw.strip_suffix(['\'','"']).unwrap_or(raw);
    let raw=if raw.is_empty() { DEFAULT_TODO_MARKDOWN_FILE } else { raw };
    let path=if Path::new(raw).is_absolute() { PathBuf::from(raw) } else { cwd.join(raw) };
    let mut resolved=PathBuf::new();
    for part in path.components() { match part { Component::CurDir=>{},Component::ParentDir=>{resolved.pop();},other=>resolved.push(other.as_os_str()) } }
    resolved
}
pub fn phases_to_markdown(phases:&[TodoPhase])->String {
    if phases.is_empty() { return format!("# {DEFAULT_INIT_PHASE}\n"); }
    let mut lines=vec![];
    for (i,p) in phases.iter().enumerate() {
        if i>0 { lines.push(String::new()); }
        lines.push(format!("# {}",p.name));
        for task in &p.tasks { let marker=match task.status { TodoStatus::Pending=>" ",TodoStatus::InProgress=>"/",TodoStatus::Completed=>"x",TodoStatus::Abandoned=>"-" }; lines.push(format!("- [{marker}] {}",task.content)); }
    }
    format!("{}\n",lines.join("\n"))
}
pub fn markdown_to_phases(markdown:&str)->TodoApplyResult {
    static HEADING:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^#{1,6}\s+(.+?)\s*$").unwrap_or_else(|e|panic!("invalid static heading regex: {e}")));
    static TASK:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^[-*+]\s*\[(.?)\]\s+(.+?)\s*$").unwrap_or_else(|e|panic!("invalid static checklist regex: {e}")));
    let mut phases:Vec<TodoPhase>=vec![];
    let mut errors=vec![];
    for (i,line) in markdown.split('\n').enumerate() {
        let trimmed=line.trim(); if trimmed.is_empty() { continue; }
        if let Some(c)=HEADING.captures(trimmed) { phases.push(TodoPhase{name:c[1].trim().into(),tasks:vec![]}); continue; }
        if let Some(c)=TASK.captures(trimmed) {
            if phases.is_empty() { phases.push(TodoPhase{name:DEFAULT_INIT_PHASE.into(),tasks:vec![]}); }
            let status=match &c[1] { " "|""=>TodoStatus::Pending,"x"|"X"=>TodoStatus::Completed,"/"|">"=>TodoStatus::InProgress,"-"|"~"=>TodoStatus::Abandoned,_=>{ errors.push(format!("Line {}: unknown status marker \"[{}]\" (use [ ], [x], [/], [-])",i+1,&c[1])); continue; } };
            let p=phases.len()-1; phases[p].tasks.push(TodoItem{content:c[2].trim().into(),status}); continue;
        }
        errors.push(format!("Line {}: unrecognized syntax \"{trimmed}\"",i+1));
    }
    normalize_in_progress_task(&mut phases);
    TodoApplyResult{phases,errors}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn checklist_roundtrip_keeps_all_statuses() { let p=vec![TodoPhase{name:"Foundation".into(),tasks:[TodoStatus::Completed,TodoStatus::InProgress,TodoStatus::Abandoned,TodoStatus::Pending].into_iter().enumerate().map(|(i,status)|TodoItem{content:format!("task {i}"),status}).collect()}]; let result=markdown_to_phases(&phases_to_markdown(&p)); assert_eq!(result.phases,p); assert!(result.errors.is_empty()); }
    #[test] fn headerless_checklist_uses_default_phase() { let result=markdown_to_phases("* [] first\n+ [X] second\n"); assert_eq!(result.phases[0].name,DEFAULT_INIT_PHASE); assert_eq!(result.phases[0].tasks[0].status,TodoStatus::InProgress); assert_eq!(result.phases[0].tasks[1].status,TodoStatus::Completed); }
    #[test] fn unknown_marker_and_unrecognized_syntax_report_lines() { let result=markdown_to_phases("# Tasks\n- [?] unknown\nordinary text\n"); assert_eq!(result.errors.len(),2); assert!(result.phases[0].tasks.is_empty()); }
    #[test] fn quoted_paths_are_resolved_lexically() { assert_eq!(resolve_todo_markdown_path(" '../TODO.md' ",Path::new("/tmp/project")),PathBuf::from("/tmp/TODO.md")); assert_eq!(resolve_todo_markdown_path("",Path::new("/tmp/project")),PathBuf::from("/tmp/project/TODO.md")); }
    #[test] fn empty_export_keeps_default_heading() { assert_eq!(phases_to_markdown(&[]),"# Tasks\n"); }
}
