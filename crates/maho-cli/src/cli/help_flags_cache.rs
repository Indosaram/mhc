use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}, time::{SystemTime, UNIX_EPOCH}};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub struct HelpFlagsScope { pub cwd: PathBuf, pub agent_dir: PathBuf, pub cli_extension_paths: Vec<PathBuf>, pub no_extensions: bool, pub project_trusted: bool }
fn cache_path(scope: &HelpFlagsScope) -> PathBuf { scope.agent_dir.join("cache/help-flags.json") }
fn scope_key(scope: &HelpFlagsScope) -> String {
    let mut material = vec![scope.cwd.to_string_lossy().into_owned(), if scope.no_extensions { "no-extensions" } else { "extensions" }.to_owned(), if scope.project_trusted { "trusted" } else { "untrusted" }.to_owned()]; material.extend(scope.cli_extension_paths.iter().map(|p| p.to_string_lossy().into_owned())); format!("{:x}", Sha256::digest(material.join("\0")))[..32].to_owned()
}
fn stamp(path: &Path) -> String { let Ok(metadata) = fs::metadata(path) else { return "absent".to_owned(); }; let milliseconds = metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|duration| duration.as_secs_f64() * 1000.0).unwrap_or(0.0); if metadata.is_dir() { format!("d:{milliseconds}") } else { format!("f:{milliseconds}:{}", metadata.len()) } }
fn read_file(scope: &HelpFlagsScope) -> Option<Value> { let value: Value = serde_json::from_slice(&fs::read(cache_path(scope)).ok()?).ok()?; (value["version"] == 1 && value["scopes"].is_object()).then_some(value) }
pub fn read_help_flags_cache(scope: &HelpFlagsScope, version: &str) -> Option<Vec<Value>> {
    let file = read_file(scope)?; let cached = &file["scopes"][scope_key(scope)]; if cached["appVersion"].as_str()? != version { return None; }
    for input in cached["inputs"].as_array()? { if stamp(Path::new(input["path"].as_str()?)) != input["state"].as_str()? { return None; } } cached["flags"].as_array().cloned()
}
pub fn write_help_flags_cache(scope: &HelpFlagsScope, flags: &[Value], extension_paths: &[PathBuf], version: &str) {
    let write = || -> std::io::Result<()> {
        let mut paths = BTreeSet::from([scope.agent_dir.join("settings.json"), scope.agent_dir.join("extensions"), scope.agent_dir.join("trust.json"), scope.cwd.join(".maho/settings.json"), scope.cwd.join(".maho/extensions")]);
        for path in extension_paths.iter().filter(|path| path.is_absolute()) { paths.insert(path.clone()); if let Some(parent) = path.parent() { paths.insert(parent.to_owned()); } }
        paths.extend(scope.cli_extension_paths.iter().filter(|p| p.is_absolute()).cloned());
        let mut scopes: BTreeMap<String, Value> = read_file(scope).and_then(|value| serde_json::from_value(value["scopes"].clone()).ok()).unwrap_or_default();
        scopes.insert(scope_key(scope), json!({"writtenAt": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64, "appVersion": version, "inputs": paths.iter().map(|path| json!({"path": path, "state": stamp(path)})).collect::<Vec<_>>(), "flags": flags}));
        let mut sorted: Vec<_> = scopes.into_iter().collect(); sorted.sort_by_key(|(_, value)| std::cmp::Reverse(value["writtenAt"].as_u64().unwrap_or(0))); let scopes: BTreeMap<_, _> = sorted.into_iter().take(8).collect();
        let path = cache_path(scope); fs::create_dir_all(path.parent().expect("cache parent"))?; let temporary = path.with_file_name(format!("help-flags.json.{}.tmp", std::process::id()));
        use std::io::Write; let mut options = fs::OpenOptions::new(); options.write(true).create(true).truncate(true); #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
        options.open(&temporary)?.write_all(serde_json::to_string(&json!({"version":1,"scopes":scopes}))?.as_bytes())?;
        if let Err(error) = fs::rename(&temporary, path) { let _ = fs::remove_file(temporary); return Err(error); } Ok(())
    }; let _ = write();
}
