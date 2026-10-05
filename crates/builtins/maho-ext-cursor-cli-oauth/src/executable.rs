use std::{collections::BTreeMap,path::Path};

#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub struct CursorAgentNotInstalledError;
impl std::fmt::Display for CursorAgentNotInstalledError {
    fn fmt(&self,f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cursor CLI is not installed. Install it with `curl https://cursor.com/install -fsS | bash`, then ensure ~/.local/bin is on your PATH.")
    }
}
impl std::error::Error for CursorAgentNotInstalledError {}
pub fn resolve_cursor_agent_executable(
    environment: &BTreeMap<String,String>, settings_path: Option<&str>, home: &Path,
    mut executable: impl FnMut(&Path) -> bool,
    mut read_directory: impl FnMut(&Path) -> std::io::Result<Vec<(String,bool)>>,
) -> Result<String,CursorAgentNotInstalledError> {
    for path in [environment.get("SENPI_CURSOR_CLI_OAUTH_EXECUTABLE").map(String::as_str),
        environment.get("CURSOR_AGENT_EXECUTABLE").map(String::as_str),settings_path].into_iter().flatten().filter(|v| !v.is_empty()) {
        if executable(Path::new(path)) { return Ok(path.into()); }
    }
    if let Some(path) = environment.get("PATH") {
        for directory in path.split(':').filter(|v| !v.is_empty()) {
            let candidate = Path::new(directory).join("cursor-agent");
            if executable(&candidate) { return Ok(candidate.to_string_lossy().into_owned()); }
        }
    }
    let versions = home.join(".local/share/cursor-agent/versions");
    if let Ok(entries) = read_directory(&versions) {
        let mut names:Vec<_> = entries.into_iter().filter(|(_,directory)| *directory).map(|(name,_)| name).collect();
        names.sort(); names.reverse();
        for name in names {
            let candidate = versions.join(name).join("cursor-agent");
            if executable(&candidate) { return Ok(candidate.to_string_lossy().into_owned()); }
        }
    }
    Err(CursorAgentNotInstalledError)
}
pub fn resolve_default(environment: &BTreeMap<String,String>, settings_path: Option<&str>, home: &Path) -> Result<String,CursorAgentNotInstalledError> {
    use std::os::unix::fs::PermissionsExt;
    resolve_cursor_agent_executable(environment,settings_path,home,|path| {
        std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    },|path| std::fs::read_dir(path)?.map(|entry| {
        let entry = entry?; Ok((entry.file_name().to_string_lossy().into_owned(),entry.file_type()?.is_dir()))
    }).collect())
}
pub async fn probe_cursor_agent_version(executable: &Path, home: &str, environment: &BTreeMap<String,String>) -> anyhow::Result<String> {
    let mut command = tokio::process::Command::new(executable);
    command.arg("--version").env_clear().envs(crate::environment::cursor_agent_environment(home,environment)).kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10),command.output()).await??;
    if !output.status.success() { anyhow::bail!("cursor-agent --version failed: {}",output.status); }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn resolve(env: &[(&str,&str)],settings:Option<&str>,available:&[&str]) -> Result<String,CursorAgentNotInstalledError> {
        let env = env.iter().map(|(k,v)| (k.to_string(),v.to_string())).collect();
        resolve_cursor_agent_executable(&env,settings,Path::new("/home/tester"),|path| available.contains(&path.to_str().unwrap()),
            |_| Ok(vec![("2026.08.10-old".into(),true),("2026.08.11-new".into(),true),("README".into(),false)]))
    }
    #[test]
    fn provider_override_first() {
        assert_eq!(resolve(&[("SENPI_CURSOR_CLI_OAUTH_EXECUTABLE","/specific"),("CURSOR_AGENT_EXECUTABLE","/fallback")],Some("/settings"),&["/specific","/fallback","/settings"]).unwrap(),"/specific");
    }
    #[test]
    fn cursor_override_second() { assert_eq!(resolve(&[("CURSOR_AGENT_EXECUTABLE","/fallback")],Some("/settings"),&["/fallback","/settings"]).unwrap(),"/fallback"); }
    #[test]
    fn settings_after_invalid_environment() {
        assert_eq!(resolve(&[("SENPI_CURSOR_CLI_OAUTH_EXECUTABLE","/missing")],Some("/settings"),&["/settings"]).unwrap(),"/settings");
    }
    #[test]
    fn path_skips_empty_entries() {
        assert_eq!(resolve(&[("PATH",":/first/bin::/second/bin:")],None,&["/second/bin/cursor-agent"]).unwrap(),"/second/bin/cursor-agent");
    }
    #[test]
    fn newest_installed_version() {
        assert_eq!(resolve(&[],None,&["/home/tester/.local/share/cursor-agent/versions/2026.08.10-old/cursor-agent","/home/tester/.local/share/cursor-agent/versions/2026.08.11-new/cursor-agent"]).unwrap(),"/home/tester/.local/share/cursor-agent/versions/2026.08.11-new/cursor-agent");
    }
    #[test]
    fn missing_binary_typed() { assert_eq!(resolve(&[],Some("/not-executable"),&[]).unwrap_err(),CursorAgentNotInstalledError); }
}
