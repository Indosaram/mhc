use std::path::{Component,Path,PathBuf};
use crate::{rule_parser::{RuleFileMeta,parse_rule_file},types::{RuleSource,TtsrRule}};
#[derive(Default)]
pub struct TtsrDiscoveryResult { pub rules:Vec<TtsrRule>,pub warnings:Vec<String> }
fn normalize(path:&Path)->PathBuf { let mut result=PathBuf::new(); for component in path.components() { match component { Component::CurDir=>{},Component::ParentDir=>{ if result.file_name().is_some_and(|name|name!="..") { result.pop(); } else if !result.has_root() { result.push(".."); } },other=>result.push(other.as_os_str()) } } result }
fn project_root(cwd:&Path,home:&Path)->PathBuf {
    let home=normalize(home); let mut current=normalize(cwd);
    for _ in 0..=100 {
        if current==home { return cwd.into(); }
        if std::fs::symlink_metadata(current.join(".senpi")).is_ok_and(|metadata|metadata.is_dir()) { return current; }
        let Some(parent)=current.parent() else { return cwd.into(); };
        if parent==current { return cwd.into(); }
        current=parent.into();
    }
    cwd.into()
}
fn load_rules(dir:&Path,source:RuleSource)->TtsrDiscoveryResult {
    let mut result=TtsrDiscoveryResult::default();
    let Ok(entries)=std::fs::read_dir(dir) else { return result; };
    let mut files=entries.filter_map(Result::ok).filter(|entry|entry.file_type().is_ok_and(|kind|kind.is_file())).filter_map(|entry|entry.file_name().into_string().ok()).filter(|name|name.ends_with(".md")).collect::<Vec<_>>(); files.sort();
    for file in files {
        let path=dir.join(&file); let name=file[..file.len()-3].to_owned();
        let bytes=match std::fs::read(&path) { Ok(bytes)=>bytes,Err(_)=>{ result.warnings.push(format!("rule \"{name}\" at {} could not be read, skipping",path.display())); continue; } };
        match parse_rule_file(&String::from_utf8_lossy(&bytes),RuleFileMeta { name,path:Some(path.to_string_lossy().into_owned()),source }) { Ok(rule)=>result.rules.push(rule),Err(skipped)=>result.warnings.push(skipped.warning) }
    }
    result
}
pub fn discover_ttsr_rules_sync(cwd:&Path,home:&Path)->TtsrDiscoveryResult {
    let global_dir=normalize(&home.join(".senpi/ttsr")); let project_dir=normalize(&project_root(cwd,home).join(".senpi/ttsr"));
    let mut global=load_rules(&global_dir,RuleSource::Global); let project=if project_dir==global_dir { TtsrDiscoveryResult::default() } else { load_rules(&project_dir,RuleSource::Project) };
    global.rules.retain(|rule|!project.rules.iter().any(|other|other.name==rule.name)); global.rules.extend(project.rules); global.warnings.extend(project.warnings); global
}
pub async fn discover_ttsr_rules(cwd:&Path,home:&Path)->TtsrDiscoveryResult { discover_ttsr_rules_sync(cwd,home) }
#[cfg(test)] mod tests {
    use super::*;
    fn write(dir:&Path,name:&str,condition:&str) { std::fs::create_dir_all(dir).unwrap(); std::fs::write(dir.join(name),format!("---\ncondition: {condition}\n---\nbody")).unwrap(); }
    #[test] fn nearest_project_overrides_global_and_files_are_sorted() { let temp=tempfile::tempdir().unwrap(); let home=temp.path().join("home"); let project=temp.path().join("project"); let cwd=project.join("nested"); std::fs::create_dir_all(&cwd).unwrap(); write(&home.join(".senpi/ttsr"),"same.md","global"); write(&home.join(".senpi/ttsr"),"a.md","first"); write(&project.join(".senpi/ttsr"),"same.md","project"); let result=discover_ttsr_rules_sync(&cwd,&home); assert_eq!(result.rules.iter().map(|rule|rule.name.as_str()).collect::<Vec<_>>(),["a","same"]); assert_eq!(result.rules[1].condition,["project"]); assert_eq!(result.rules[1].source,RuleSource::Project); }
    #[test] fn home_boundary_prevents_project_origin_duplicate() { let temp=tempfile::tempdir().unwrap(); write(&temp.path().join(".senpi/ttsr"),"test.md","x"); let result=discover_ttsr_rules_sync(temp.path(),temp.path()); assert_eq!(result.rules.len(),1); assert_eq!(result.rules[0].source,RuleSource::Global); }
    #[test] fn symlink_rule_files_are_excluded() { let temp=tempfile::tempdir().unwrap(); let rules=temp.path().join(".senpi/ttsr"); write(&rules,"real.md","x"); std::os::unix::fs::symlink(rules.join("real.md"),rules.join("link.md")).unwrap(); let result=discover_ttsr_rules_sync(temp.path(),temp.path()); assert_eq!(result.rules.len(),1); }
    #[test] fn skipped_project_rule_does_not_shadow_valid_global_rule() { let temp=tempfile::tempdir().unwrap(); let home=temp.path().join("home"); let project=temp.path().join("project"); write(&home.join(".senpi/ttsr"),"same.md","x"); write(&project.join(".senpi/ttsr"),"same.md","'('"); let result=discover_ttsr_rules_sync(&project,&home); assert_eq!(result.rules.len(),1); assert_eq!(result.rules[0].source,RuleSource::Global); assert_eq!(result.warnings.len(),1); }
}
