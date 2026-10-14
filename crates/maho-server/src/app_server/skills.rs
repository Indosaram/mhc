use super::{registry::{JsonRpcError, MethodRegistration, MethodRegistry, MethodScope}, thread_registry::ThreadRegistry};
use maho_core::{package_identity::dedupe_paths_by_package_identity, package_manager::{DefaultPackageManager, PackageManagerOptions}, paths::{PathInputOptions, canonicalize_path, resolve_path}, settings_manager::SettingsManager, skills::{LoadSkillsOptions, LoadSkillsResult, load_skills}, source_info::{SourceScope, create_source_info}};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

pub type SkillLoader = Arc<dyn Fn(&str, &str) -> Result<LoadSkillsResult, String> + Send + Sync>;
pub fn register_skill_methods(registry: &mut MethodRegistry, agent_dir: String, server_cwd: String, threads: Arc<ThreadRegistry>) {
    register_skill_methods_with_loader(registry,agent_dir,server_cwd,threads,Arc::new(|cwd,agent_dir|Ok(load_skills(&LoadSkillsOptions {cwd:cwd.into(),agent_dir:agent_dir.into(),skill_paths:Vec::new(),include_defaults:true}))));
}
/// Pinned runtime `resourceLoaderFactory`: a fresh `DefaultResourceLoader({cwd, agentDir})` + `reload()`.
/// Skill paths come from `SettingsManager.create(cwd, agentDir)` (project trusted by default) resolved
/// through `DefaultPackageManager.resolve()` (settings paths, installed packages, auto-discovered dirs),
/// merged with `additional_skill_paths`, deduped by package identity and loaded without defaults.
/// Runs on a blocking-pool thread (the handler spawns it there) so it can drive the async resolve.
pub fn resource_skill_loader(additional_skill_paths: Vec<String>) -> SkillLoader {
    Arc::new(move |cwd, agent_dir| {
        let mut settings = SettingsManager::create(cwd, agent_dir, &maho_core::config::home_dir(), true);
        let manager = DefaultPackageManager::new(PackageManagerOptions { cwd, agent_dir, settings_manager: &mut settings });
        let handle = tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
        let resolved = handle.block_on(manager.resolve(None)).map_err(|error| error.to_string())?;
        let enabled = resolved.skills.into_iter().filter(|resource| resource.enabled).collect::<Vec<_>>();
        let options = PathInputOptions { trim: true, ..Default::default() };
        let mut seen = std::collections::HashSet::new();
        let merged = enabled.iter().map(|resource| resource.path.clone()).chain(additional_skill_paths.iter().cloned())
            .map(|path| resolve_path(&path, cwd, &options))
            .filter(|path| seen.insert(canonicalize_path(path)))
            .collect::<Vec<_>>();
        let skill_paths = dedupe_paths_by_package_identity(&merged);
        let mut loaded = load_skills(&LoadSkillsOptions { cwd: cwd.into(), agent_dir: agent_dir.into(), skill_paths, include_defaults: false });
        for skill in &mut loaded.skills {
            let path = std::path::Path::new(&skill.file_path);
            let metadata = enabled.iter().find(|resource| path == std::path::Path::new(&resource.path))
                .or_else(|| enabled.iter().find(|resource| path.starts_with(&resource.path)))
                .map(|resource| &resource.metadata);
            if let Some(metadata) = metadata { skill.source_info = create_source_info(&skill.file_path, metadata); }
        }
        Ok(loaded)
    })
}
pub fn register_skill_methods_with_loader(registry: &mut MethodRegistry, agent_dir: String, server_cwd: String, threads: Arc<ThreadRegistry>, loader: SkillLoader) {
    let cache = Arc::new(Mutex::new(BTreeMap::<String, LoadSkillsResult>::new()));
    registry.register("skills/list".into(), MethodRegistration {
        requires_init: true, experimental: false, scope: MethodScope::Global,
        handler: Arc::new(move |context| {
            let cache = cache.clone();
            let agent_dir = agent_dir.clone();
            let server_cwd = server_cwd.clone();
            let threads = threads.clone();
            let loader = loader.clone();
            Box::pin(async move {
                let params = &context.request["params"];
                if !params.is_null() && !params.is_object() {
                    return Err(JsonRpcError::new(-32602, "Invalid params"));
                }
                if params.get("forceReload").is_some_and(|value| !value.is_boolean()) {
                    return Err(JsonRpcError::new(-32602, "Invalid params"));
                }
                let cwds = match params.get("cwds") {
                    None => Vec::new(),
                    Some(Value::Array(values)) if values.iter().all(Value::is_string) => values.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                    Some(_) => return Err(JsonRpcError::new(-32602, "Invalid params")),
                };
                let cwds = if cwds.is_empty() { vec![server_cwd.clone()] } else { cwds };
                let mut data = Vec::new();
                for cwd in cwds {
                    let cwd = resolve_path(&cwd, &server_cwd, &PathInputOptions { trim: true, ..Default::default() });
                    let error = match std::fs::metadata(&cwd) {
                        Ok(metadata) if metadata.is_dir() => None,
                        Ok(_) => Some("skill cwd is not a directory".to_owned()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some("skill cwd does not exist".to_owned()),
                        Err(error) => Some(error.to_string()),
                    };
                    if let Some(message) = error {
                        data.push(json!({"cwd":cwd,"skills":[],"errors":[{"path":cwd,"message":message}]}));
                        continue;
                    }
                    let mut cache = cache.lock().await;
                    let loaded = if params["forceReload"] == true || !cache.contains_key(&cwd) {
                        match loaded_thread_skills(&threads,&cwd,params["forceReload"] == true).await {
                            Some(loaded)=>loaded,
                            None=>{
                                let loader = loader.clone();let load_cwd = cwd.clone();let agent_dir = agent_dir.clone();
                                let res = tokio::task::spawn_blocking(move || loader(&load_cwd,&agent_dir)).await.unwrap_or_else(|error| Err(error.to_string()));
                                match res {
                                Ok(loaded)=>{cache.insert(cwd.clone(),loaded.clone());loaded},
                                Err(message)=>{cache.remove(&cwd);data.push(json!({"cwd":cwd,"skills":[],"errors":[{"path":cwd,"message":message}]}));continue;},
                            }},
                        }
                    } else {cache[&cwd].clone()};
                    let skills = loaded.skills.iter().map(|skill| {
                        let scope = match skill.source_info.scope {
                            SourceScope::User => "user",
                            SourceScope::Project => "repo",
                            SourceScope::Temporary | SourceScope::System => "system",
                        };
                        json!({"name":skill.name,"description":skill.description,"path":skill.file_path,"scope":scope,"enabled":!skill.disable_model_invocation})
                    }).collect::<Vec<_>>();
                    let errors = loaded.diagnostics.iter().map(|diagnostic| json!({"path":diagnostic.path.as_deref().unwrap_or(&cwd),"message":diagnostic.message})).collect::<Vec<_>>();
                    data.push(json!({"cwd":cwd,"skills":skills,"errors":errors}));
                }
                Ok(json!({"data":data}))
            })
        }),
    });
}
/// Pinned `findLoadedLoader`: among the loaded threads (insertion order), the first whose resolved
/// cwd equals `cwd`; return its live per-session skill snapshot. `None` when no loaded thread
/// matches or it has not loaded skills yet, so the caller falls back to a fresh disk load.
async fn loaded_thread_skills(threads: &Arc<ThreadRegistry>, cwd: &str, force_reload: bool) -> Option<LoadSkillsResult> {
    if force_reload { return None; }
    for thread in threads.list_loaded().await {
        let Some(id) = thread["id"].as_str() else { continue };
        let thread_cwd = resolve_path(thread["cwd"].as_str().unwrap_or_default(), cwd, &PathInputOptions { trim: true, ..Default::default() });
        if thread_cwd != cwd { continue; }
        if let Ok(entry) = threads.get_loaded_thread(id).await {
            let entry = entry.lock().await;
            if let Some(loaded) = entry.session.loaded_skills() { return Some(loaded); }
        }
    }
    None
}
