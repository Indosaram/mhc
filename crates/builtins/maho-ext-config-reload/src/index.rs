use std::collections::BTreeMap;
use serde_json::Value;
use std::{path::{Path, PathBuf}, sync::Arc};
use crate::{watch_engine::{WatchTarget, WatchKind, WatchFilter}, extension_watch_scope::{is_loadable_extension_entry, is_scannable_extension_directory}};

pub struct ConfigReloadHandoffRegistry<T> { handoffs: BTreeMap<String, T> }
impl<T> Default for ConfigReloadHandoffRegistry<T> { fn default() -> Self { Self { handoffs: BTreeMap::new() } } }
impl<T> ConfigReloadHandoffRegistry<T> {
    pub fn set(&mut self, session_handle: String, handoff: T) { self.handoffs.insert(session_handle, handoff); }
    pub fn take(&mut self, session_handle: &str) -> Option<T> { self.handoffs.remove(session_handle) }
    pub fn delete(&mut self, session_handle: &str) { self.handoffs.remove(session_handle); }
}
pub struct ResolvedConfigReloadSettings { pub enabled: bool, pub debounce_ms: f64, pub watch: BTreeMap<String, bool> }
pub struct ActiveTarget { pub registration_id: String, pub target: WatchTarget, pub rearm_on_creation: Option<PathBuf> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingChange { pub registration_id: String, pub paths: Vec<PathBuf> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadAdmission { Empty, InFlight, Busy, Compacting, Unavailable, ProbeVeto }
pub fn reload_admission(pending_empty: bool, in_flight: bool, idle: bool, pending_messages: bool, compacting: bool, request_available: bool) -> ReloadAdmission {
    if pending_empty { ReloadAdmission::Empty }
    else if in_flight { ReloadAdmission::InFlight }
    else if !idle || pending_messages { ReloadAdmission::Busy }
    else if compacting { ReloadAdmission::Compacting }
    else if !request_available { ReloadAdmission::Unavailable }
    else { ReloadAdmission::ProbeVeto }
}
#[derive(Default)]
pub struct PendingChanges { changes: BTreeMap<String, std::collections::BTreeSet<PathBuf>> }
impl PendingChanges {
    pub fn add(&mut self, registration_id: &str, paths: &[PathBuf]) { self.changes.entry(registration_id.into()).or_default().extend(paths.iter().cloned()); }
    pub fn delete(&mut self, registration_id: &str) { self.changes.remove(registration_id); }
    pub fn clear(&mut self) { self.changes.clear(); }
    pub fn is_empty(&self) -> bool { self.changes.is_empty() }
    pub fn snapshot(&self) -> Vec<PendingChange> { self.changes.iter().map(|(id, paths)| PendingChange { registration_id: id.clone(), paths: paths.iter().cloned().collect() }).collect() }
}
pub fn compare_snapshots(previous: &BTreeMap<PathBuf, String>, next: &BTreeMap<PathBuf, String>) -> Vec<PathBuf> {
    previous.keys().chain(next.keys()).filter(|path| previous.get(*path) != next.get(*path)).cloned().collect::<std::collections::BTreeSet<_>>().into_iter().collect()
}
pub fn significant_changed_paths(paths: &[PathBuf], snapshot: &BTreeMap<PathBuf, String>, settings_contents: &mut BTreeMap<PathBuf, String>, agent_dir: &Path, cwd: &Path, logger: &mut crate::log::ConfigReloadLogger) -> Vec<PathBuf> {
    use crate::{log::{LogEvent, LogLevel}, routine_settings::{is_settings_path, update_settings_content_snapshot, exclude_routine_only_settings_changes}};
    let watched: Vec<_> = paths.iter().filter(|path| {
        if !is_settings_path(path, agent_dir, cwd) { return true; }
        if !snapshot.get(*path).is_some_and(|hash| maho_core::settings_manager::was_self_write(&path.to_string_lossy(), hash)) { return true; }
        logger.log(LogLevel::Debug, LogEvent::SelfWriteSuppressed { path: &path.to_string_lossy() });
        update_settings_content_snapshot(settings_contents, path);
        false
    }).cloned().collect();
    let significant = exclude_routine_only_settings_changes(&watched, settings_contents, agent_dir, cwd, logger);
    let config = crate::generated_shim_filter::exclude_generated_extension_shims(&significant, agent_dir);
    for path in &significant { if !config.contains(path) { logger.log(LogLevel::Debug, LogEvent::GeneratedShimChangeSuppressed { path: &path.to_string_lossy() }); } }
    config
}
pub fn build_external_watch_targets(cwd: &Path, registrations: &[crate::protocol::ConfigWatchRegistration]) -> Vec<ActiveTarget> {
    use crate::protocol::{ConfigWatchTargetKind, matches_config_watch_filter};
    let mut targets = Vec::new();
    for registration in registrations {
        for (index, target) in registration.targets.iter().enumerate() {
            let raw = Path::new(target.path.trim());
            let absolute = if raw.is_absolute() { raw.to_path_buf() } else { cwd.join(raw) };
            let mut path = PathBuf::new();
            for component in absolute.components() { match component { std::path::Component::ParentDir => { path.pop(); }, std::path::Component::CurDir => {}, other => path.push(other.as_os_str()) } }
            let filters = target.filter_globs.clone().unwrap_or_default();
            let (path, kind, allow_list, filter) = if target.kind == ConfigWatchTargetKind::File {
                let name = path.file_name().unwrap_or_default().to_owned();
                let allowed = vec![PathBuf::from(&name)];
                let filter: WatchFilter = Arc::new(move |relative| relative == Path::new(&name) && matches_config_watch_filter(&relative.to_string_lossy(), &filters));
                (path.parent().unwrap_or(&path).to_path_buf(), WatchKind::Dir, Some(allowed), filter)
            } else {
                let literal: Vec<_> = filters.iter().map(|filter| filter.strip_prefix('/').unwrap_or(filter)).filter(|filter| !filter.contains('*') && !filter.contains('/') && !filter.contains("\\\\")).map(PathBuf::from).collect();
                let allowed = (!literal.is_empty()).then_some(literal);
                let filter: WatchFilter = Arc::new(move |relative| matches_config_watch_filter(&relative.to_string_lossy(), &filters));
                (path, WatchKind::DirRecursive, allowed, filter)
            };
            targets.push(ActiveTarget { registration_id: registration.id.clone(), target: WatchTarget { id: format!("external-{}-{index}", registration.id), path, kind, allow_list, filter: Some(filter) }, rearm_on_creation: None });
        }
    }
    targets
}
pub fn group_changed_paths(paths: &[PathBuf], targets: &[ActiveTarget]) -> BTreeMap<String, Vec<PathBuf>> {
    let mut groups: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for path in paths {
        let mut matched = false;
        for active in targets {
            let target = &active.target;
            let Ok(relative) = path.strip_prefix(&target.path) else { continue; };
            if (target.kind == WatchKind::Dir && relative.components().count() > 1)
                || target.allow_list.as_ref().is_some_and(|allowed| !allowed.iter().any(|allowed| relative == allowed || relative.starts_with(allowed)))
                || target.filter.as_ref().is_some_and(|filter| !filter(relative)) { continue; }
            let group = groups.entry(active.registration_id.clone()).or_default();
            if !group.contains(path) { group.push(path.clone()); }
            matched = true;
        }
        if !matched { groups.entry("builtin".into()).or_default().push(path.clone()); }
    }
    groups
}
pub fn validate_builtin_paths(paths: &[PathBuf], agent_dir: &Path, cwd: &Path) -> Vec<String> {
    let mut errors = Vec::new();
    for path in paths {
        if !path.exists() { continue; }
        if crate::routine_settings::is_settings_path(path, agent_dir, cwd) {
            let parsed = std::fs::read_to_string(path).map_err(|error| error.to_string()).and_then(|content| maho_core::settings_manager::parse_settings_json(&content).map_err(|error| error.to_string()));
            if let Err(error) = parsed { errors.push(format!("Invalid {}: {error}", path.file_name().unwrap_or_default().to_string_lossy())); }
        } else if path == &agent_dir.join("models.json") {
            if let Some(error) = maho_core::model_config::ModelConfig::load_sync(Some(path)).get_error() { errors.push(error.into()); }
        } else if path == &agent_dir.join("keybindings.json") {
            match std::fs::read_to_string(path).map_err(|error| error.to_string()).and_then(|content| serde_json::from_str::<Value>(&content).map_err(|error| error.to_string())) {
                Ok(Value::Object(bindings)) => { if bindings.values().any(|value| !value.is_string() && !value.as_array().is_some_and(|values| values.iter().all(Value::is_string))) { errors.push("keybindings.json bindings must be strings or string arrays".into()); } },
                Ok(_) => errors.push("keybindings.json must contain an object".into()),
                Err(error) => errors.push(format!("Invalid keybindings.json: {error}")),
            }
        }
    }
    errors
}
pub fn build_builtin_watch_targets(cwd: &Path, agent_dir: &Path, project_trusted: bool, settings: &ResolvedConfigReloadSettings, skill_paths: &[PathBuf]) -> Vec<ActiveTarget> {
    let mut targets = Vec::new();
    let json: Vec<_> = ["settings.jsonc", "settings.json", "models.json", "keybindings.json"].into_iter().filter(|name| {
        settings.watch[if name.starts_with("settings.") { "settings" } else if *name == "models.json" { "models" } else { "keybindings" }]
    }).map(PathBuf::from).collect();
    if !json.is_empty() { targets.push(active("builtin-global-json", agent_dir, WatchKind::Dir, Some(json), None, None)); }
    for (resource, id) in [("prompts", "builtin-global-prompts"), ("extensions", "builtin-global-extensions")] {
        if settings.watch[resource] { add_directory(&mut targets, id, &agent_dir.join(resource), resource == "extensions"); }
    }
    if settings.watch["skills"] {
        for (index, path) in skill_paths.iter().enumerate() {
            let path = if path.is_absolute() { path.clone() } else { cwd.join(path) };
            let id = format!("builtin-skill-{index}");
            if std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
                if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) { targets.push(active(&id, parent, WatchKind::Dir, Some(vec![name.into()]), None, None)); }
            } else { add_directory(&mut targets, &id, &path, false); }
        }
    }
    if project_trusted {
        let project = crate::routine_settings::join_config_dir(cwd);
        if settings.watch.values().any(|enabled| *enabled) { targets.push(active("builtin-project-presence", cwd, WatchKind::Dir, Some(vec![maho_core::config::config_dir_name().into()]), None, (!is_existing_directory(&project)).then(|| project.clone()))); }
        if is_existing_directory(&project) {
            if settings.watch["settings"] { targets.push(active("builtin-project-settings", &project, WatchKind::Dir, Some(vec!["settings.jsonc".into(), "settings.json".into()]), None, None)); }
            for resource in ["prompts", "skills", "extensions"] { if settings.watch[resource] { add_directory(&mut targets, &format!("builtin-project-{resource}"), &project.join(resource), resource == "extensions"); } }
        }
    }
    targets
}
fn active(id: &str, path: &Path, kind: WatchKind, allow_list: Option<Vec<PathBuf>>, filter: Option<WatchFilter>, rearm_on_creation: Option<PathBuf>) -> ActiveTarget {
    ActiveTarget { registration_id: "builtin".into(), target: WatchTarget { id: id.into(), path: path.into(), kind, allow_list, filter }, rearm_on_creation }
}
fn add_directory(targets: &mut Vec<ActiveTarget>, id: &str, path: &Path, extensions: bool) {
    if is_existing_directory(path) {
        let root = path.to_path_buf();
        let filter: Option<WatchFilter> = extensions.then(|| Arc::new(move |relative: &Path| is_loadable_extension_entry(&root, &relative.to_string_lossy()) || is_scannable_extension_directory(&root, &relative.to_string_lossy())) as WatchFilter);
        targets.push(active(id, path, WatchKind::DirRecursive, None, filter, None));
    } else {
        let mut parent = path.parent().unwrap_or(path).to_path_buf();
        while !is_existing_directory(&parent) { let Some(next) = parent.parent() else { break; }; parent = next.into(); }
        if let Some(segment) = path.strip_prefix(&parent).ok().and_then(|relative| relative.components().next()) {
            let segment = PathBuf::from(segment.as_os_str());
            targets.push(active(&format!("{id}-presence"), &parent, WatchKind::Dir, Some(vec![segment.clone()]), None, Some(parent.join(segment))));
        }
    }
}
fn is_existing_directory(path: &Path) -> bool { std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) }
pub fn resolve_config_reload_settings(global: &Value, project: &Value) -> ResolvedConfigReloadSettings {
    let global = global.get("configReload").filter(|value| value.is_object());
    let project = project.get("configReload").filter(|value| value.is_object());
    let enabled = project.and_then(|patch| patch.get("enabled")).and_then(Value::as_bool).or_else(|| global.and_then(|patch| patch.get("enabled")).and_then(Value::as_bool)).unwrap_or(true);
    let debounce = |patch: Option<&Value>| patch.and_then(|patch| patch.get("debounceMs")).and_then(Value::as_f64).filter(|value| value.is_finite() && *value >= 0.0).map(f64::floor);
    let watch = ["settings", "models", "keybindings", "prompts", "skills", "extensions"].into_iter().map(|key| {
        let field = |patch: Option<&Value>| patch.and_then(|patch| patch.get("watch")).and_then(|watch| watch.get(key)).and_then(Value::as_bool);
        (key.into(), field(project).or_else(|| field(global)).unwrap_or(true))
    }).collect();
    ResolvedConfigReloadSettings { enabled, debounce_ms: debounce(project).or_else(|| debounce(global)).unwrap_or(200.0), watch }
}
