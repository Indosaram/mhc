use std::{collections::BTreeMap,path::{Path,PathBuf},sync::Mutex};
fn parsed(text:Option<&str>)->Option<[f64;3]> {
    let capture=regex::Regex::new(r"([0-9]+)\.([0-9]+)\.([0-9]+)").expect("version pattern").captures(text?)?;
    Some([capture[1].parse().ok()?,capture[2].parse().ok()?,capture[3].parse().ok()?])
}
pub fn is_newer(candidate:Option<&str>,baseline:Option<&str>)->bool {
    match (parsed(candidate),parsed(baseline)) {(Some(left),Some(right))=>left>right,_=>false}
}
#[derive(Default)]
pub struct VersionProbe {cache:Mutex<BTreeMap<PathBuf,Option<String>>>}
impl VersionProbe {
    pub async fn probe(&self,executable:&Path)->Option<String> {
        if let Some(cached)=self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(executable).cloned() {return cached;}
        let mut command=tokio::process::Command::new(executable);command.arg("--version").stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
        let version=match tokio::time::timeout(std::time::Duration::from_secs(5),command.output()).await {
            Ok(Ok(output)) if output.status.success()=> {let text=String::from_utf8_lossy(&output.stdout);regex::Regex::new(r"[0-9]+\.[0-9]+\.[0-9]+").expect("version pattern").find(&text).map(|v|v.as_str().to_owned())},_=>None,
        };
        self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(executable.into(),version.clone());version
    }
}
pub fn bundled_version(manifest:&Path)->Option<String> {serde_json::from_slice::<serde_json::Value>(&std::fs::read(manifest).ok()?).ok()?["claudeCodeVersion"].as_str().map(str::to_owned)}
#[cfg(test)]
mod tests {
    use super::*;use std::os::unix::fs::PermissionsExt;
    #[test]
    fn compares_only_parsed_semantic_triplets() {assert!(is_newer(Some("Claude Code 2.1.100"),Some("2.1.99")));assert!(!is_newer(Some("2.1.100"),Some("2.1.100")));assert!(!is_newer(Some("bad"),Some("2.1.1")));assert!(!is_newer(Some("2.1.1"),None));}
    #[tokio::test]
    async fn probe_caches_success_and_failure_by_path() {
        let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("claude");std::fs::write(&executable,"#!/bin/sh\nprintf '2.1.101 (Claude Code)\\n'\n").expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");let probe=VersionProbe::default();assert_eq!(probe.probe(&executable).await.as_deref(),Some("2.1.101"));std::fs::write(&executable,"#!/bin/sh\nexit 1\n").expect("replace");assert_eq!(probe.probe(&executable).await.as_deref(),Some("2.1.101"));let failing=directory.path().join("bad");assert!(probe.probe(&failing).await.is_none());
    }
}
