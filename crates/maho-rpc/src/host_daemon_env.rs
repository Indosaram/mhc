use std::collections::{BTreeMap,BTreeSet};

pub fn daemon_env_is_allowed(name:&str,platform:&str) -> bool {
    let upper;
    let name=if platform=="win32" { upper=name.to_uppercase();upper.as_str() }else{name};
    if platform=="win32" && ["SYSTEMROOT","SYSTEMDRIVE","WINDIR","COMSPEC","PATHEXT","TEMP","TMP","USERPROFILE","APPDATA","LOCALAPPDATA","PROGRAMFILES","PROGRAMFILES(X86)","PROGRAMDATA","HOMEDRIVE","HOMEPATH","NUMBER_OF_PROCESSORS","PROCESSOR_ARCHITECTURE","OS"].contains(&name) { return true; }
    ["PATH","HOME","USER","LOGNAME","SHELL","TMPDIR","TERM","LANG","HTTP_PROXY","HTTPS_PROXY","NO_PROXY"].contains(&name)
        || ["LC_","XDG_","SENPI_","OMO_","PI_","ANTHROPIC_","OPENAI_","GOOGLE_","GEMINI_","AZURE_","AWS_","OPENROUTER_","XAI_","MISTRAL_","DEEPSEEK_","GROQ_","CEREBRAS_","MINIMAX_"].iter().any(|prefix| name.starts_with(prefix))
        || name.strip_suffix("_API_KEY").is_some_and(|prefix| !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_uppercase()||byte.is_ascii_digit()||byte==b'_'))
}
pub fn daemon_env_overrides(process_env:&BTreeMap<String,Option<String>>,spec_env:&BTreeMap<String,String>,platform:&str) -> BTreeMap<String,Option<String>> {
    let mut overrides=process_env.keys().filter(|name| !daemon_env_is_allowed(name,platform)).map(|name| (name.clone(),None)).collect::<BTreeMap<_,_>>();
    overrides.extend(spec_env.iter().map(|(name,value)| (name.clone(),Some(value.clone()))));overrides
}
pub fn daemon_env_keys(process_env:&BTreeMap<String,Option<String>>,spec_env:&BTreeMap<String,String>,platform:&str) -> Vec<String> {
    process_env.keys().filter(|name| daemon_env_is_allowed(name,platform)).chain(spec_env.keys()).cloned().collect::<BTreeSet<_>>().into_iter().collect()
}
pub fn write_daemon_env_keys(paths:&crate::host_daemon_paths::HostDaemonPaths,keys:&[String]) -> Result<(),crate::host_daemon_paths::HostDaemonStateError> {
    crate::host_daemon_state::write_state_file(&paths.dir.join("env-keys.json"),&serde_json::json!({"env_keys":keys}))
}
pub fn read_daemon_env_keys(paths:&crate::host_daemon_paths::HostDaemonPaths) -> std::io::Result<Vec<String>> {
    let text=crate::host_daemon_state::read_file_or_undefined(&paths.dir.join("env-keys.json"))?;
    let parsed=crate::host_daemon_state::parse_json(text.as_deref());
    Ok(parsed.as_ref().and_then(|record|record.get("env_keys")).and_then(serde_json::Value::as_array).map(|keys|keys.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect()).unwrap_or_default())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn persisted_keys_ignore_non_strings(){let temp=tempfile::tempdir().unwrap();let paths=crate::host_daemon_paths::create_host_daemon_paths("socket",temp.path());crate::host_daemon_paths::create_daemon_directories(&paths).unwrap();assert!(read_daemon_env_keys(&paths).unwrap().is_empty());write_daemon_env_keys(&paths,&["PATH".into()]).unwrap();assert_eq!(read_daemon_env_keys(&paths).unwrap(),vec!["PATH"]);crate::host_daemon_state::write_state_file(&paths.dir.join("env-keys.json"),&serde_json::json!({"env_keys":[false,"HOME",1]})).unwrap();assert_eq!(read_daemon_env_keys(&paths).unwrap(),vec!["HOME"]);}
    #[test] fn posix_is_case_sensitive_and_windows_keeps_wiring(){assert!(!daemon_env_is_allowed("Path","linux"));assert!(daemon_env_is_allowed("Path","win32"));assert!(daemon_env_is_allowed("SystemRoot","win32"));assert!(!daemon_env_is_allowed("SystemRoot","linux"));}
    #[test] fn exact_provider_and_credential_patterns(){for name in ["SENPI_RPC_HOST","OMO_X","PI_X","AWS_REGION","CUSTOM_API_KEY","LC_ALL","XDG_DATA_HOME"]{assert!(daemon_env_is_allowed(name,"linux"),"{name}");}for name in ["CI_TOKEN","DATABASE_URL","_API_KEY","lower_API_KEY","MOM_X","OPENAIKEY"]{assert!(!daemon_env_is_allowed(name,"linux"),"{name}");}}
    #[test] fn explicit_spec_wins_and_unknown_values_are_dropped(){let process=BTreeMap::from([("PATH".into(),Some("path".into())),("CI_TOKEN".into(),Some("value".into())),("DATABASE_URL".into(),None)]);let spec=BTreeMap::from([("CI_TOKEN".into(),"explicit".into())]);assert_eq!(daemon_env_overrides(&process,&spec,"linux"),BTreeMap::from([("CI_TOKEN".into(),Some("explicit".into())),("DATABASE_URL".into(),None)]));assert_eq!(daemon_env_keys(&process,&spec,"linux"),vec!["CI_TOKEN","PATH"]);}
}
