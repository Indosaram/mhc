use std::{fs, io, path::{Path,PathBuf}};
use crate::git_helpers::run;
fn resolve_exclude_path(root: &Path) -> Option<PathBuf> { run(root,&["rev-parse","--git-path","info/exclude"]).ok().map(|s|root.join(s.trim())) }
fn read_content(path: &Path) -> String { fs::read_to_string(path).unwrap_or_default() }
pub fn add_local_exclude_paths(root: &Path, paths: &[&str]) -> io::Result<()> {
    let Some(path)=resolve_exclude_path(root) else { return Ok(()); };
    let content=read_content(&path);
    let mut lines: Vec<_>=content.split('\n').map(str::to_owned).collect();
    for p in paths { if !lines.iter().any(|l|l==p) { if lines.last().is_some_and(|s|!s.is_empty()) { lines.push(String::new()); } lines.push((*p).into()); } }
    let updated=lines.join("\n");
    if updated!=content { if let Some(parent)=path.parent() { fs::create_dir_all(parent)?; } fs::write(path,updated)?; }
    Ok(())
}
pub fn remove_local_exclude_paths(root: &Path, paths: &[&str]) -> io::Result<()> {
    let Some(path)=resolve_exclude_path(root) else { return Ok(()); };
    let content=read_content(&path);
    if content.is_empty() { return Ok(()); }
    let updated=content.split('\n').filter(|l|!paths.contains(l)).collect::<Vec<_>>().join("\n");
    if updated!=content { if let Some(parent)=path.parent() { fs::create_dir_all(parent)?; } fs::write(path,updated)?; }
    Ok(())
}
pub fn is_excluded(root: &Path, path: &str) -> bool { resolve_exclude_path(root).is_some_and(|p|read_content(&p).split('\n').any(|l|l.trim()==path)) }
#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_helpers::tests::repo;
    fn exclude(root: &Path) -> PathBuf { root.join(".git/info/exclude") }
    #[test] fn adds_paths() { let (t,_)=repo(); add_local_exclude_paths(t.path(),&["AGENTS.md",".omo/init-deep.json"]).unwrap(); assert!(is_excluded(t.path(),"AGENTS.md")); assert!(is_excluded(t.path(),".omo/init-deep.json")); }
    #[test] fn no_duplicates() { let (t,_)=repo(); for _ in 0..2 { add_local_exclude_paths(t.path(),&["AGENTS.md",".omo/init-deep.json"]).unwrap(); } let content=read_content(&exclude(t.path())); assert_eq!(content.split('\n').filter(|s|*s=="AGENTS.md").count(),1); assert_eq!(content.split('\n').filter(|s|*s==".omo/init-deep.json").count(),1); }
    #[test] fn preserves_user_lines() { let (t,_)=repo(); fs::write(exclude(t.path()),"user-file.txt\n.env.local\n").unwrap(); add_local_exclude_paths(t.path(),&["AGENTS.md"]).unwrap(); for p in ["user-file.txt",".env.local","AGENTS.md"] { assert!(is_excluded(t.path(),p)); } }
    #[test] fn removes_only_named_lines() { let (t,_)=repo(); fs::write(exclude(t.path()),"user-file.txt\nAGENTS.md\n.omo/init-deep.json\n.env.local").unwrap(); remove_local_exclude_paths(t.path(),&["AGENTS.md",".omo/init-deep.json"]).unwrap(); assert!(!is_excluded(t.path(),"AGENTS.md")); assert!(!is_excluded(t.path(),".omo/init-deep.json")); assert!(is_excluded(t.path(),"user-file.txt")); assert!(is_excluded(t.path(),".env.local")); }
    #[test] fn removal_absent_file() { let (t,_)=repo(); fs::remove_file(exclude(t.path())).unwrap(); remove_local_exclude_paths(t.path(),&["AGENTS.md"]).unwrap(); }
    #[test] fn finds_present() { let (t,_)=repo(); add_local_exclude_paths(t.path(),&["AGENTS.md"]).unwrap(); assert!(is_excluded(t.path(),"AGENTS.md")); }
    #[test] fn absent_path() { let (t,_)=repo(); add_local_exclude_paths(t.path(),&["AGENTS.md"]).unwrap(); assert!(!is_excluded(t.path(),"not-excluded.txt")); }
    #[test] fn absent_file() { let (t,_)=repo(); fs::remove_file(exclude(t.path())).unwrap(); assert!(!is_excluded(t.path(),"AGENTS.md")); }
    #[test] fn add_nonrepo() { let t=tempfile::tempdir().unwrap(); add_local_exclude_paths(t.path(),&["AGENTS.md"]).unwrap(); assert!(!t.path().join(".git").exists()); }
    #[test] fn remove_nonrepo() { let t=tempfile::tempdir().unwrap(); remove_local_exclude_paths(t.path(),&["AGENTS.md"]).unwrap(); }
    #[test] fn check_nonrepo() { let t=tempfile::tempdir().unwrap(); assert!(!is_excluded(t.path(),"AGENTS.md")); }
}
