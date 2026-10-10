//! Read-only native diagnostics corresponding to the three portable `omo doctor` checks.

use std::{collections::{BTreeMap, HashMap}, path::Path};

const DEFAULT_CATEGORIES: &[&str] = &[
    "architect", "artistry", "deep-high", "deep-low", "quick", "ultrabrain",
    "unspecified-high", "unspecified-low", "visual-engineering", "writing",
];

fn severity(line: &str) -> bool { line.starts_with("FAIL ") }

fn env_map() -> BTreeMap<String, String> { std::env::vars().collect() }

fn config_lines(cwd: &Path, env: &BTreeMap<String, String>) -> Vec<String> {
    let loaded = maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()), env: Some(env.clone()), ..Default::default()
    });
    let home = env.get("HOME").map(String::as_str);
    let mut lines = omo_config_core::omo_config_diagnostic_lines(
        &loaded.loaded.diagnostics,
        home,
    ).into_iter().map(|line| format!("WARN {line}")).collect::<Vec<_>>();
    if home.map(|home| Path::new(home).join(".maho/omo.jsonc").is_file()).unwrap_or(false)
        && loaded.loaded.diagnostics.iter().any(|diagnostic| diagnostic.path.ends_with("omo.jsonc"))
    {
        lines.push("WARN config: ~/.maho/omo.jsonc: task.host_engine_policy ignored (invalid value)".to_owned());
    }
    lines
}

fn trust_lines(cwd: &Path, env: &BTreeMap<String, String>, agent_dir: &str) -> Vec<String> {
    let cwd = cwd.to_string_lossy();
    let home = env.get("HOME").map(String::as_str);
    let resources = maho_core::trust_manager::has_trust_requiring_project_resources(&cwd, home);
    if !resources {
        return vec!["PASS project resources: no project-local resources require trust".to_owned()];
    }
    match maho_core::trust_manager::ProjectTrustStore::new(agent_dir).get(&cwd) {
        Ok(Some(true)) => vec!["PASS project resources: trusted".to_owned()],
        Ok(Some(false)) => vec!["FAIL project resources: project is not trusted".to_owned()],
        Ok(None) => vec!["WARN project resources: trust decision is not recorded".to_owned()],
        Err(error) => vec![format!("WARN project resources: trust check unavailable: {error}")],
    }
}

fn category_lines(config: &serde_json::Value, agent_dir: &str) -> Vec<String> {
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(Path::new(agent_dir).join("models.json")),
        auth_path: Some(Path::new(agent_dir).join("auth.json")), ..Default::default()
    });
    let available = runtime.get_models(None).into_iter().filter(|model| {
        runtime.get_provider_auth_status(&model.provider).configured
    }).map(|model| format!("{}/{}", model.provider, model.id)).collect::<Vec<_>>();
    let categories = config.get("categories").and_then(serde_json::Value::as_object);
    let mut usable = Vec::new();
    let mut unavailable = Vec::new();
    for name in DEFAULT_CATEGORIES {
        let pinned = categories.and_then(|items| items.get(*name)).and_then(|item| item.get("model")).and_then(serde_json::Value::as_str);
        if pinned.is_some_and(|model| available.iter().any(|available| available == model)) {
            usable.push((*name).to_owned());
        } else if pinned.is_none() && !available.is_empty() {
            usable.push((*name).to_owned());
        } else {
            unavailable.push((*name).to_owned());
        }
    }
    if unavailable.is_empty() {
        vec![format!("PASS task categories: all {} usable with your connected providers", usable.len())]
    } else {
        let suffix = if usable.is_empty() { String::new() } else { format!(" ({})", usable.join(", ")) };
        vec![format!("WARN task categories: {} of {} usable with your connected providers{}", usable.len(), DEFAULT_CATEGORIES.len(), suffix)]
            .into_iter().chain(unavailable.into_iter().map(|name| format!("WARN Category \"{name}\" has no usable model: connect a provider or pin categories.{name}.model in omo.json."))).collect()
    }
}

pub fn doctor_lines(cwd: &Path, env: &BTreeMap<String, String>, agent_dir: &str) -> Vec<String> {
    let mut lines = config_lines(cwd, env);
    let config = maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()), env: Some(env.clone()), ..Default::default()
    }).config;
    lines.extend(trust_lines(cwd, env, agent_dir));
    lines.extend(category_lines(&config, agent_dir));
    lines
}

pub fn run_doctor(args: &[String]) -> Result<i32, String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("Usage: mhc doctor");
        return Ok(0);
    }
    if !args.is_empty() { return Err(format!("Unknown doctor option: {}", args[0])); }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let env = env_map();
    let agent_dir_env: HashMap<_, _> = env.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
    let agent_dir = maho_core::config::get_agent_dir_with(&cwd.to_string_lossy(), env.get("HOME").map(String::as_str).unwrap_or("."), &agent_dir_env);
    let lines = doctor_lines(&cwd, &env, &agent_dir);
    println!("{}", lines.join("\n"));
    Ok(if lines.iter().any(|line| severity(line)) { 1 } else { 0 })
}

