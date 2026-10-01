use serde_json::Value;
pub const CONFIG_WATCH_REGISTER: &str = "config-watch:register";
pub const CONFIG_WATCH_UNREGISTER: &str = "config-watch:unregister";
pub const CONFIG_WATCH_READY: &str = "config-watch:ready";
pub const CONFIG_WATCH_CHANGED: &str = "config-watch:changed";
pub const CONFIG_WATCH_RELOADED: &str = "config-watch:reloaded";
pub const CONFIG_WATCH_REJECTED: &str = "config-watch:rejected";
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigWatchTargetKind { File, Dir }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWatchTarget { pub path: String, pub kind: ConfigWatchTargetKind, pub filter_globs: Option<Vec<String>> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigWatchRegistration { pub id: String, pub display_name: String, pub targets: Vec<ConfigWatchTarget> }
fn strings(value: &Value) -> Option<Vec<String>> { value.as_array()?.iter().map(|entry| entry.as_str().map(str::to_owned)).collect() }
pub fn parse_config_watch_target(value: &Value) -> Option<ConfigWatchTarget> {
 let object = value.as_object()?;
 Some(ConfigWatchTarget { path: object.get("path")?.as_str()?.into(), kind: match object.get("kind")?.as_str()? { "file" => ConfigWatchTargetKind::File, "dir" => ConfigWatchTargetKind::Dir, _ => return None }, filter_globs: match object.get("filterGlobs") { Some(value) => Some(strings(value)?), None => None } })
}
pub fn parse_config_watch_registration(value: &Value) -> Option<ConfigWatchRegistration> {
 let object = value.as_object()?;
 if object.contains_key("validate") { return None; }
 Some(ConfigWatchRegistration { id: object.get("id")?.as_str()?.into(), display_name: object.get("displayName")?.as_str()?.into(), targets: object.get("targets")?.as_array()?.iter().map(parse_config_watch_target).collect::<Option<_>>()? })
}
pub fn is_config_watch_validation(value: &Value) -> bool {
 match value.get("ok").and_then(Value::as_bool) { Some(true) => true, Some(false) => value.get("errors").and_then(strings).is_some(), None => false }
}
pub fn is_config_watch_unregistration(value: &Value) -> bool { value.get("id").is_some_and(Value::is_string) }
pub fn is_config_watch_reloaded(value: &Value) -> bool { value.get("registrationId").is_some_and(Value::is_string) && value.get("paths").and_then(strings).is_some() }
pub fn is_config_watch_changed(value: &Value) -> bool { is_config_watch_reloaded(value) && value.get("deferred").is_some_and(Value::is_boolean) }
pub fn is_config_watch_rejected(value: &Value) -> bool { is_config_watch_reloaded(value) && value.get("errors").and_then(strings).is_some() }
pub fn matches_config_watch_filter(path: &str, filters: &[String]) -> bool {
 if filters.is_empty() { return true; }
 let normalized = path.replace('\\', "/");
 let basename = normalized.rsplit('/').next().unwrap_or("");
 filters.iter().any(|filter| {
  if let Some(anchored) = filter.strip_prefix('/') { normalized == anchored }
  else if let Some(suffix) = filter.strip_prefix('*') { normalized.ends_with(suffix) }
  else { basename == filter || normalized.ends_with(&format!("/{filter}")) }
 })
}
pub fn resolve_config_watch_registrations(registrations: impl IntoIterator<Item = ConfigWatchRegistration>) -> Vec<ConfigWatchRegistration> {
 let mut resolved: Vec<ConfigWatchRegistration> = Vec::new();
 for registration in registrations {
  if let Some(existing) = resolved.iter_mut().find(|existing| existing.id == registration.id) { *existing = registration; }
  else { resolved.push(registration); }
 }
 resolved
}
