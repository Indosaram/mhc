//! Read-only native diagnostics for configuration, trust, task categories, and package integrity.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};

fn severity(line: &str) -> bool {
    line.starts_with("FAIL ")
}

#[doc(hidden)]
pub fn package_lines(directory: &Path) -> Vec<String> {
    use sha2::{Digest, Sha256};
    let manifest_path = directory.join("manifest.json");
    if !manifest_path.exists() {
        return vec!["WARN native package: manifest unavailable".into()];
    }
    let manifest: serde_json::Value = match std::fs::read(&manifest_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(manifest) => manifest,
        None => return vec!["FAIL native package: invalid manifest".into()],
    };
    let Some(executables) = manifest
        .get("executables")
        .and_then(serde_json::Value::as_array)
    else {
        return vec!["FAIL native package: executable manifest unavailable".into()];
    };
    let mut lines = Vec::new();
    for executable in executables {
        let Some(path) = executable.get("path").and_then(serde_json::Value::as_str) else {
            lines.push("FAIL native package: executable path missing".into());
            continue;
        };
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            lines.push("FAIL native package: executable path escapes package".into());
            continue;
        }
        let expected = executable.get("sha256").and_then(serde_json::Value::as_str);
        let valid = std::fs::read(directory.join(relative))
            .ok()
            .is_some_and(|bytes| {
                let actual = format!("{:x}", Sha256::digest(bytes));
                expected == Some(actual.as_str())
            });
        lines.push(format!(
            "{} native executable: {path}",
            if valid { "PASS" } else { "FAIL" }
        ));
    }
    lines
}

fn env_map() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn loaded_config(
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> maho_omo_config_resolution::SenpiOmoConfigResult {
    maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {
        cwd: Some(cwd.to_string_lossy().into_owned()),
        env: Some(env.clone()),
        ..Default::default()
    })
}

fn config_lines(
    loaded: &maho_omo_config_resolution::SenpiOmoConfigResult,
    env: &BTreeMap<String, String>,
) -> Vec<String> {
    let home = env.get("HOME").map(String::as_str);
    omo_config_core::omo_config_diagnostic_lines(&loaded.loaded.diagnostics, home)
        .into_iter()
        .map(|line| format!("WARN {line}"))
        .collect()
}

fn trust_lines(cwd: &Path, env: &BTreeMap<String, String>, agent_dir: &str) -> Vec<String> {
    let cwd = cwd.to_string_lossy();
    let home = env.get("HOME").map(String::as_str);
    let resources = maho_core::trust_manager::has_trust_requiring_project_resources(&cwd, home);
    if !resources {
        return vec!["PASS project resources: no project-local resources require trust".to_owned()];
    }
    let trust_path = Path::new(agent_dir).join("trust.json");
    let trust = match maho_core::trust_manager::read_trust_file(&trust_path.to_string_lossy()) {
        Ok(data) => maho_core::trust_manager::find_nearest_trust_entry(&data, &cwd)
            .map(|entry| entry.decision),
        Err(_) => {
            return vec![
                "WARN project resources: trust store is invalid or unavailable".to_owned(),
            ];
        }
    };
    match trust {
        Some(true) => vec!["PASS project resources: trusted".to_owned()],
        Some(false) => vec!["FAIL project resources: project is not trusted".to_owned()],
        None => vec!["WARN project resources: trust decision is not recorded".to_owned()],
    }
}

fn ast_grep_lines() -> Vec<String> {
    // Use the MCP server's resolver so doctor reports the binary ast-grep tools will use.
    match omo_utils::ast_grep::resolve_sg_binary_sync(
        &omo_utils::ast_grep::SgResolverOptions::default(),
    ) {
        omo_utils::ast_grep::SgResolution::Found { path, tier } => {
            let layer = match tier {
                omo_utils::ast_grep::SgResolutionTier::EnvOverride => "environment override",
                omo_utils::ast_grep::SgResolutionTier::OmoRuntime => "OMO runtime",
                omo_utils::ast_grep::SgResolutionTier::SkillBin => "skill bin",
                omo_utils::ast_grep::SgResolutionTier::Path => "PATH",
                omo_utils::ast_grep::SgResolutionTier::Homebrew => "Homebrew",
            };
            vec![format!("PASS ast-grep: {path} (found via {layer})")]
        }
        omo_utils::ast_grep::SgResolution::NotFound { error } => {
            // Missing ast-grep only disables its tools; it does not prevent the session working.
            let mut lines = vec![
                "WARN ast-grep: binary unavailable; install ast-grep or configure OMO_AST_GREP_SG_PATH"
                    .to_owned(),
            ];
            lines.extend(
                error
                    .hints
                    .into_iter()
                    .map(|hint| format!("  ast-grep hint: {hint}")),
            );
            lines
        }
    }
}

fn category_lines(config: &serde_json::Value, agent_dir: &str) -> Vec<String> {
    let auth_path = Path::new(agent_dir).join("auth.json");
    // The engine's store reads auth.json with `parse_auth_document`, so pooled entries whose tokens live only in
    // `accounts[]` count as connected exactly as they do for a session.
    let credentials = if auth_path.exists() {
        match std::fs::read_to_string(&auth_path)
            .ok()
            .and_then(|content| {
                maho_core::auth_store_load_error::parse_auth_document(&content).ok()
            }) {
            Some(data) => maho_core::auth_storage::repair_poisoned_pool_slots(&data).0,
            None => return vec!["FAIL credentials: invalid auth storage".to_owned()],
        }
    } else {
        Default::default()
    };
    let runtime = match maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(Path::new(agent_dir).join("models.json")),
            credentials: Some(std::sync::Arc::new(
                maho_core::auth_storage::AuthStorage::in_memory(credentials),
            )),
            ..Default::default()
        },
    ) {
        Ok(runtime) => runtime,
        Err(error) => {
            return vec![format!(
                "FAIL task categories: could not construct model runtime: {error}"
            )];
        }
    };
    let (runtime, failures) =
        super::extension_providers::register_builtin_extension_providers(runtime, None);
    let registration_failures = failures
        .into_iter()
        .map(|(provider, error)| {
            format!("WARN task categories: could not register provider {provider}: {error}")
        })
        .collect::<Vec<_>>();
    let available = runtime
        .get_models(None)
        .into_iter()
        .filter(|model| runtime.get_provider_auth_status(&model.provider).configured)
        .map(|model| serde_json::json!(model))
        .collect::<Vec<_>>();
    category_coverage(config, available, registration_failures)
}

fn category_coverage(
    config: &serde_json::Value,
    available: Vec<serde_json::Value>,
    registration_failures: Vec<String>,
) -> Vec<String> {
    struct Registry(Vec<serde_json::Value>);
    impl senpi_task::host::SenpiModelRegistry for Registry {
        fn get_available(&self) -> Result<serde_json::Value, senpi_task::host::HostError> {
            Ok(serde_json::Value::Array(self.0.clone()))
        }
        fn find(&self, provider: &str, id: &str) -> Option<serde_json::Value> {
            self.0
                .iter()
                .find(|model| model["provider"] == provider && model["id"] == id)
                .cloned()
        }
    }
    fn unlisted_provider_model(
        chain: &[senpi_task::DelegateFallbackEntry],
        available: &[String],
    ) -> Option<String> {
        let available_models = available.iter().cloned().collect::<indexmap::IndexSet<_>>();
        let deps = delegate_core::DelegateModelResolutionDeps {
            connected_providers: None,
            has_provider_models_cache: true,
            has_connected_providers_cache: true,
            log: None,
        };
        chain.iter().find_map(|rung| {
            let result = delegate_core::resolve_model_for_delegate_task(
                &delegate_core::DelegateModelResolutionInput {
                    fallback_chain: Some(vec![delegate_core::DelegateFallbackEntry {
                        providers: rung.providers.clone(),
                        model: rung.model.clone(),
                        variant: rung.variant.clone(),
                    }]),
                    available_models: available_models.clone(),
                    ..Default::default()
                },
                &deps,
            );
            match result {
                Some(delegate_core::DelegateModelResolutionResult::Resolved(model)) => {
                    Some(model.model)
                }
                _ => None,
            }
        })
    }
    let available_model_names = available
        .iter()
        .filter_map(|model| {
            Some(format!(
                "{}/{}",
                model.get("provider")?.as_str()?,
                model.get("id")?.as_str()?
            ))
        })
        .collect::<Vec<_>>();
    let registry = Registry(available);
    let mut usable = Vec::new();
    let mut unavailable = Vec::new();
    let mut names = senpi_task::category::BUILTIN_CATEGORY_DEFAULTS
        .iter()
        .map(|definition| definition.name.to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(custom) = config
        .get("categories")
        .and_then(serde_json::Value::as_object)
    {
        names.extend(custom.keys().cloned());
    }
    for name in &names {
        match senpi_task::category::resolve_category(name, config, &registry, &Default::default()) {
            Ok(senpi_task::category::CategoryResolutionResult::Resolved { .. }) => {
                usable.push(name.clone())
            }
            Ok(senpi_task::category::CategoryResolutionResult::Disabled { .. }) => {}
            Ok(senpi_task::category::CategoryResolutionResult::ModelUnavailable(details)) => {
                let providers = details.missing_providers.unwrap_or_default();
                let unlisted = details
                    .attempted_chain
                    .as_deref()
                    .and_then(|chain| unlisted_provider_model(chain, &available_model_names));
                let notice = if let Some(model) = unlisted {
                    let gateway = model
                        .split_once('/')
                        .map_or(model.as_str(), |(provider, _)| provider);
                    format!(
                        "Category \"{name}\" is unavailable: only {gateway}, which its chain does not list, serves its models, and omo never bills a builtin category to an unlisted provider. Connect one of its providers ({}) with /login <provider>, or opt in with categories.{name}.model = \"{model}\" in omo.json.",
                        providers.join(", ")
                    )
                } else if providers.is_empty() {
                    format!(
                        "Category \"{name}\" has no usable model: your connected providers serve none of its fallback-chain models. Pin categories.{name}.model in omo.json."
                    )
                } else {
                    format!(
                        "Category \"{name}\" has no usable model: none of its fallback-chain providers are connected ({}). Connect one with /login <provider>, or pin categories.{name}.model in omo.json.",
                        providers.join(", ")
                    )
                };
                unavailable.push(notice);
            }
            Ok(senpi_task::category::CategoryResolutionResult::NotFound { .. }) => {}
            Err(_) => return Vec::new(),
        }
    }
    if unavailable.is_empty() && registration_failures.is_empty() {
        vec![format!(
            "PASS task categories: all {} usable with your connected providers",
            usable.len()
        )]
    } else {
        let suffix = if usable.is_empty() {
            String::new()
        } else {
            format!(" ({})", usable.join(", "))
        };
        let mut lines = vec![format!(
            "WARN task categories: {} of {} usable with your connected providers{}",
            usable.len(),
            usable.len() + unavailable.len(),
            suffix
        )];
        lines.extend(registration_failures);
        lines.extend(
            unavailable
                .into_iter()
                .map(|notice| format!("WARN {notice}")),
        );
        lines
    }
}

pub fn doctor_lines(cwd: &Path, env: &BTreeMap<String, String>, agent_dir: &str) -> Vec<String> {
    let loaded = loaded_config(cwd, env);
    let mut lines = config_lines(&loaded, env);
    lines.extend(trust_lines(cwd, env, agent_dir));
    lines.extend(category_lines(&loaded.config, agent_dir));
    lines.extend(ast_grep_lines());
    lines
}

pub fn run_doctor(args: &[String]) -> Result<i32, String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("Usage: mhc doctor");
        return Ok(0);
    }
    if !args.is_empty() {
        return Err(format!("Unknown doctor option: {}", args[0]));
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let env = env_map();
    let agent_dir_env: HashMap<_, _> = env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let agent_dir = maho_core::config::get_agent_dir_with(
        &cwd.to_string_lossy(),
        env.get("HOME").map(String::as_str).unwrap_or("."),
        &agent_dir_env,
    );
    let lines = doctor_lines(&cwd, &env, &agent_dir);
    let mut lines = lines;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    lines.extend(package_lines(
        executable
            .parent()
            .ok_or("Executable directory unavailable")?,
    ));
    println!("{}", lines.join("\n"));
    Ok(if lines.iter().any(|line| severity(line)) {
        1
    } else {
        0
    })
}

#[cfg(test)]
mod routing_tests {
    use super::category_coverage;
    use serde_json::json;

    #[test]
    fn doctor_counts_all_ten_builtins_and_only_exact_gpt_lanes() {
        let report = |id| {
            category_coverage(
                &json!({}),
                vec![json!({"provider":"openai", "id": id})],
                vec![],
            )
        };
        assert_eq!(
            report("gpt-6.2-astra")[0],
            "WARN task categories: 2 of 10 usable with your connected providers (deep-high, ultrabrain)"
        );
        assert_eq!(
            report("gpt-6.2-sol")[0],
            "WARN task categories: 2 of 10 usable with your connected providers (deep-low, unspecified-low)"
        );
        assert!(report("gpt-6.2-sol-fast")[0].starts_with("WARN task categories: 0 of 10 "));
    }
}
