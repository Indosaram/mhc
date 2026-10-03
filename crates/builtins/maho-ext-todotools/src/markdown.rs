use std::path::{Component,Path,PathBuf};
use regex::Regex;
use std::sync::LazyLock;
use crate::{todo_types::*,todo_query::normalize_in_progress_task,todo_operations::TodoApplyResult};
pub const DEFAULT_TODO_MARKDOWN_FILE:&str="TODO.md";
fn js_whitespace(c:char)->bool { matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
pub fn resolve_todo_markdown_path(input:&str,cwd:&Path)->PathBuf {
    let raw=input.trim_matches(js_whitespace);
    let raw=raw.strip_prefix(['\'','"']).unwrap_or(raw);
    let raw=raw.strip_suffix(['\'','"']).unwrap_or(raw);
    let raw=if raw.is_empty() { DEFAULT_TODO_MARKDOWN_FILE } else { raw };
    if Path::new(raw).is_absolute() { return PathBuf::from(raw); }
    let base=if cwd.is_absolute() { cwd.to_path_buf() } else { std::env::current_dir().expect("process cwd").join(cwd) };
    let path=base.join(raw);
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
    static HEADING:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^#{1,6}[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+([^\r\n\x{2028}\x{2029}]+?)[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*$").unwrap_or_else(|e|panic!("invalid static heading regex: {e}")));
    static TASK:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^[-*+][\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*\[([^\r\n\x{2028}\x{2029}]?)\][\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]+([^\r\n\x{2028}\x{2029}]+?)[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]*$").unwrap_or_else(|e|panic!("invalid static checklist regex: {e}")));
    let mut phases:Vec<TodoPhase>=vec![];
    let mut errors=vec![];
    for (i,line) in markdown.split('\n').enumerate() {
        let trimmed=line.trim_matches(js_whitespace); if trimmed.is_empty() { continue; }
        if let Some(c)=HEADING.captures(trimmed) { phases.push(TodoPhase{name:c[1].trim_matches(js_whitespace).into(),tasks:vec![]}); continue; }
        if let Some(c)=TASK.captures(trimmed).filter(|c|c[1].encode_utf16().count()<=1) {
            if phases.is_empty() { phases.push(TodoPhase{name:DEFAULT_INIT_PHASE.into(),tasks:vec![]}); }
            let status=match &c[1] { " "|""=>TodoStatus::Pending,"x"|"X"=>TodoStatus::Completed,"/"|">"=>TodoStatus::InProgress,"-"|"~"=>TodoStatus::Abandoned,_=>{ errors.push(format!("Line {}: unknown status marker \"[{}]\" (use [ ], [x], [/], [-])",i+1,&c[1])); continue; } };
            let p=phases.len()-1; phases[p].tasks.push(TodoItem{content:c[2].trim_matches(js_whitespace).into(),status}); continue;
        }
        errors.push(format!("Line {}: unrecognized syntax \"{trimmed}\"",i+1));
    }
    normalize_in_progress_task(&mut phases);
    TodoApplyResult{phases,errors}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn astral_status_marker_is_not_a_single_javascript_code_unit() { let result=markdown_to_phases("- [\u{1f600}] task"); assert!(result.phases.is_empty()); assert_eq!(result.errors.len(),1); }
    #[test] fn javascript_regex_whitespace_accepts_bom_but_not_next_line() { let accepted=markdown_to_phases("#\u{feff}Tasks\n-\u{feff}[ ]\u{feff}one"); assert!(accepted.errors.is_empty()); assert_eq!(accepted.phases[0].tasks[0].content,"one"); assert_eq!(markdown_to_phases("#\u{0085}Tasks").errors.len(),1); }
    #[test] fn relative_cwd_resolves_against_process_cwd() { assert_eq!(resolve_todo_markdown_path("TODO.md",Path::new("relative")),std::env::current_dir().unwrap().join("relative/TODO.md")); }
    #[test] fn absolute_user_paths_preserve_dot_segments() { assert_eq!(resolve_todo_markdown_path("/tmp/../TODO.md",Path::new("/other")),PathBuf::from("/tmp/../TODO.md")); }
    #[test] fn bom_wrapped_markdown_is_trimmed_like_javascript() { let result=markdown_to_phases("\u{feff}# Tasks\u{feff}\n\u{feff}- [ ] one\u{feff}"); assert!(result.errors.is_empty()); assert_eq!(result.phases[0].tasks[0].content,"one"); }
    #[test] fn checklist_roundtrip_keeps_all_statuses() { let p=vec![TodoPhase{name:"Foundation".into(),tasks:[TodoStatus::Completed,TodoStatus::InProgress,TodoStatus::Abandoned,TodoStatus::Pending].into_iter().enumerate().map(|(i,status)|TodoItem{content:format!("task {i}"),status}).collect()}]; let result=markdown_to_phases(&phases_to_markdown(&p)); assert_eq!(result.phases,p); assert!(result.errors.is_empty()); }
    #[test] fn headerless_checklist_uses_default_phase() { let result=markdown_to_phases("* [] first\n+ [X] second\n"); assert_eq!(result.phases[0].name,DEFAULT_INIT_PHASE); assert_eq!(result.phases[0].tasks[0].status,TodoStatus::InProgress); assert_eq!(result.phases[0].tasks[1].status,TodoStatus::Completed); }
    #[test] fn unknown_marker_and_unrecognized_syntax_report_lines() { let result=markdown_to_phases("# Tasks\n- [?] unknown\nordinary text\n"); assert_eq!(result.errors.len(),2); assert!(result.phases[0].tasks.is_empty()); }
    #[test] fn quoted_paths_are_resolved_lexically() { assert_eq!(resolve_todo_markdown_path(" '../TODO.md' ",Path::new("/tmp/project")),PathBuf::from("/tmp/TODO.md")); assert_eq!(resolve_todo_markdown_path("",Path::new("/tmp/project")),PathBuf::from("/tmp/project/TODO.md")); }
    #[test] fn empty_export_keeps_default_heading() { assert_eq!(phases_to_markdown(&[]),"# Tasks\n"); }
}
