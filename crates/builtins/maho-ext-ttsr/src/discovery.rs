use std::path::{Component,Path,PathBuf};
use crate::{rule_parser::{RuleFileMeta,parse_rule_file},types::{RuleSource,TtsrRule}};
#[derive(Default)]
pub struct TtsrDiscoveryResult { pub rules:Vec<TtsrRule>,pub warnings:Vec<String> }
pub fn discover_ttsr_rules_sync_default(cwd:&Path)->TtsrDiscoveryResult {
    let home=dirs::home_dir().expect("operating system home directory is unavailable");
    discover_ttsr_rules_sync(cwd,&home)
}
pub async fn discover_ttsr_rules_default(cwd:&Path)->TtsrDiscoveryResult {
    let home=dirs::home_dir().expect("operating system home directory is unavailable");
    discover_ttsr_rules(cwd,&home).await
}
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
    let mut files=entries.filter_map(Result::ok).filter(|entry|entry.file_type().is_ok_and(|kind|kind.is_file())).filter_map(|entry|entry.file_name().into_string().ok()).filter(|name|name.ends_with(".md")).collect::<Vec<_>>(); files.sort_by(|left,right|left.encode_utf16().cmp(right.encode_utf16()));
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
async fn project_root_async(cwd:&Path,home:&Path)->PathBuf {
    let home=normalize(home); let mut current=normalize(cwd);
    for _ in 0..=100 {
        if current==home { return cwd.into(); }
        if tokio::fs::symlink_metadata(current.join(".senpi")).await.is_ok_and(|metadata|metadata.is_dir()) { return current; }
        let Some(parent)=current.parent() else { return cwd.into(); };
        if parent==current { return cwd.into(); }
        current=parent.into();
    }
    cwd.into()
}
async fn load_rules_async(dir:&Path,source:RuleSource)->TtsrDiscoveryResult {
    let mut result=TtsrDiscoveryResult::default();
    let Ok(mut entries)=tokio::fs::read_dir(dir).await else { return result; };
    let mut files=Vec::new();
    while let Ok(Some(entry))=entries.next_entry().await {
        if entry.file_type().await.is_ok_and(|kind|kind.is_file()) && let Ok(name)=entry.file_name().into_string() && name.ends_with(".md") { files.push(name); }
    }
    files.sort_by(|left,right|left.encode_utf16().cmp(right.encode_utf16()));
    for file in files {
        let path=dir.join(&file); let name=file[..file.len()-3].to_owned();
        let bytes=match tokio::fs::read(&path).await { Ok(bytes)=>bytes,Err(_)=>{ result.warnings.push(format!("rule \"{name}\" at {} could not be read, skipping",path.display())); continue; } };
        match parse_rule_file(&String::from_utf8_lossy(&bytes),RuleFileMeta { name,path:Some(path.to_string_lossy().into_owned()),source }) { Ok(rule)=>result.rules.push(rule),Err(skipped)=>result.warnings.push(skipped.warning) }
    }
    result
}
pub async fn discover_ttsr_rules(cwd:&Path,home:&Path)->TtsrDiscoveryResult {
    let global_dir=normalize(&home.join(".senpi/ttsr")); let project_dir=normalize(&project_root_async(cwd,home).await.join(".senpi/ttsr"));
    let mut global=load_rules_async(&global_dir,RuleSource::Global).await;
    let project=if project_dir==global_dir { TtsrDiscoveryResult::default() } else { load_rules_async(&project_dir,RuleSource::Project).await };
    global.rules.retain(|rule|!project.rules.iter().any(|other|other.name==rule.name)); global.rules.extend(project.rules); global.warnings.extend(project.warnings); global
}
#[cfg(test)] mod tests {
    use super::*;
    fn write(dir:&Path,name:&str,condition:&str) { std::fs::create_dir_all(dir).unwrap(); std::fs::write(dir.join(name),format!("---\ncondition: {condition}\n---\nbody")).unwrap(); }
    #[tokio::test] async fn omitted_home_uses_native_operating_system_resolution() {
        let temp=tempfile::tempdir().unwrap(); let home=dirs::home_dir().unwrap();
        write(&temp.path().join(".senpi/ttsr"),"native-home-fixture.md","fixture");
        let expected=discover_ttsr_rules_sync(temp.path(),&home);
        let sync=discover_ttsr_rules_sync_default(temp.path()); let asynchronous=discover_ttsr_rules_default(temp.path()).await;
        assert_eq!(sync.rules,expected.rules); assert_eq!(sync.warnings,expected.warnings);
        assert_eq!(asynchronous.rules,expected.rules); assert_eq!(asynchronous.warnings,expected.warnings);
    }
    #[tokio::test] async fn filenames_follow_javascript_utf16_sort_order() {
        let temp=tempfile::tempdir().unwrap(); let home=temp.path().join("home"); let cwd=temp.path().join("project"); std::fs::create_dir_all(&cwd).unwrap();
        let rules=home.join(".senpi/ttsr"); write(&rules,"\u{e000}.md","x"); write(&rules,"\u{10000}.md","x");
        let sync=discover_ttsr_rules_sync(&cwd,&home); assert_eq!(sync.rules.iter().map(|rule|rule.name.as_str()).collect::<Vec<_>>(),["\u{10000}","\u{e000}"]);
        assert_eq!(discover_ttsr_rules(&cwd,&home).await.rules,sync.rules);
    }
    #[test] fn nearest_project_overrides_global_and_files_are_sorted() { let temp=tempfile::tempdir().unwrap(); let home=temp.path().join("home"); let project=temp.path().join("project"); let cwd=project.join("nested"); std::fs::create_dir_all(&cwd).unwrap(); write(&home.join(".senpi/ttsr"),"same.md","global"); write(&home.join(".senpi/ttsr"),"a.md","first"); write(&project.join(".senpi/ttsr"),"same.md","project"); let result=discover_ttsr_rules_sync(&cwd,&home); assert_eq!(result.rules.iter().map(|rule|rule.name.as_str()).collect::<Vec<_>>(),["a","same"]); assert_eq!(result.rules[1].condition,["project"]); assert_eq!(result.rules[1].source,RuleSource::Project); }
    #[test] fn home_boundary_prevents_project_origin_duplicate() { let temp=tempfile::tempdir().unwrap(); write(&temp.path().join(".senpi/ttsr"),"test.md","x"); let result=discover_ttsr_rules_sync(temp.path(),temp.path()); assert_eq!(result.rules.len(),1); assert_eq!(result.rules[0].source,RuleSource::Global); }
    #[test] fn symlink_rule_files_are_excluded() { let temp=tempfile::tempdir().unwrap(); let rules=temp.path().join(".senpi/ttsr"); write(&rules,"real.md","x"); std::os::unix::fs::symlink(rules.join("real.md"),rules.join("link.md")).unwrap(); let result=discover_ttsr_rules_sync(temp.path(),temp.path()); assert_eq!(result.rules.len(),1); }
    #[test] fn skipped_project_rule_does_not_shadow_valid_global_rule() { let temp=tempfile::tempdir().unwrap(); let home=temp.path().join("home"); let project=temp.path().join("project"); write(&home.join(".senpi/ttsr"),"same.md","x"); write(&project.join(".senpi/ttsr"),"same.md","'('"); let result=discover_ttsr_rules_sync(&project,&home); assert_eq!(result.rules.len(),1); assert_eq!(result.rules[0].source,RuleSource::Global); assert_eq!(result.warnings.len(),1); }
    #[tokio::test] async fn async_discovery_matches_sync_precedence_warnings_and_symlinks() {
        let temp=tempfile::tempdir().unwrap(); let home=temp.path().join("home"); let project=temp.path().join("project");
        write(&home.join(".senpi/ttsr"),"same.md","global"); write(&home.join(".senpi/ttsr"),"a.md","first");
        let rules=project.join(".senpi/ttsr"); write(&rules,"same.md","project"); write(&rules,"invalid.md","'('");
        std::os::unix::fs::symlink(rules.join("same.md"),rules.join("link.md")).unwrap();
        let nested=project.join("nested"); std::fs::create_dir_all(&nested).unwrap();
        let expected=discover_ttsr_rules_sync(&nested,&home); let actual=discover_ttsr_rules(&nested,&home).await;
        assert_eq!(actual.rules,expected.rules); assert_eq!(actual.warnings,expected.warnings);
    }
    #[tokio::test] async fn upstream_missing_directories_are_empty() {
        let temp=tempfile::tempdir().unwrap(); let result=discover_ttsr_rules(&temp.path().join("stray/nested"),&temp.path().join("home")).await; assert!(result.rules.is_empty()); assert!(result.warnings.is_empty());
    }
    #[tokio::test] async fn upstream_nearest_ancestor_wins() {
        let temp=tempfile::tempdir().unwrap(); let outer=temp.path().join("outer"); let inner=outer.join("inner"); let cwd=inner.join("src/pkg"); std::fs::create_dir_all(&cwd).unwrap();
        write(&outer.join(".senpi/ttsr"),"outer.md","outer"); write(&inner.join(".senpi/ttsr"),"inner.md","inner");
        let result=discover_ttsr_rules(&cwd,&temp.path().join("home")).await; assert_eq!(result.rules.len(),1); assert_eq!(result.rules[0].name,"inner"); assert_eq!(result.rules[0].path.as_deref(),inner.join(".senpi/ttsr/inner.md").to_str());
    }
    #[tokio::test] async fn upstream_global_then_project_alphabetical_order() {
        let temp=tempfile::tempdir().unwrap(); let home=temp.path().join("home"); let project=temp.path().join("project");
        for (root,name) in [(&home,"charlie.md"),(&home,"alpha.md"),(&project,"zebra.md"),(&project,"bravo.md")] { write(&root.join(".senpi/ttsr"),name,"x"); }
        let result=discover_ttsr_rules(&project,&home).await; assert_eq!(result.rules.iter().map(|rule|rule.name.as_str()).collect::<Vec<_>>(),["alpha","charlie","bravo","zebra"]);
    }
    #[tokio::test] async fn upstream_malformed_and_unreachable_siblings_warn_without_hiding_valid_rule() {
        let temp=tempfile::tempdir().unwrap(); let home=temp.path().join("home"); let project=temp.path().join("project"); let rules=project.join(".senpi/ttsr");
        write(&rules,"good.md","x"); write(&rules,"broken.md","'['");
        std::fs::write(rules.join("conditionless.md"),"---\ndescription: no condition\n---\nbody").unwrap();
        std::fs::write(rules.join("unreachable.md"),"---\ncondition: x\nscope: '???'\n---\nbody").unwrap();
        let result=discover_ttsr_rules(&project,&home).await; assert_eq!(result.rules.len(),1); assert_eq!(result.rules[0].name,"good"); assert_eq!(result.warnings.len(),3);
    }
}
