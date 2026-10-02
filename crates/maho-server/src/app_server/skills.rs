use super::registry::{JsonRpcError, MethodRegistration, MethodRegistry, MethodScope};
use maho_core::{paths::{PathInputOptions, resolve_path}, skills::{LoadSkillsOptions, LoadSkillsResult, load_skills}, source_info::SourceScope};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::Mutex;

pub fn register_skill_methods(registry: &mut MethodRegistry, agent_dir: String, server_cwd: String) {
    let cache = Arc::new(Mutex::new(BTreeMap::<String, LoadSkillsResult>::new()));
    registry.register("skills/list".into(), MethodRegistration {
        requires_init: true, experimental: false, scope: MethodScope::Global,
        handler: Arc::new(move |context| {
            let cache = cache.clone();
            let agent_dir = agent_dir.clone();
            let server_cwd = server_cwd.clone();
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
                    if params["forceReload"] == true || !cache.contains_key(&cwd) {
                        cache.insert(cwd.clone(), load_skills(&LoadSkillsOptions { cwd: cwd.clone(), agent_dir: agent_dir.clone(), skill_paths: Vec::new(), include_defaults: true }));
                    }
                    let loaded = &cache[&cwd];
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
