//! `mhc doctor` in the MIT profile (`--no-default-features`): a read-only report of the harness
//! configuration (sources, load diagnostics, config profile) and of the session-start model
//! profile resolution, built on the clean-room `maho-config` and `maho-model-select` crates.
//! The `omo` build keeps its own `doctor` module.

use maho_model_select::{
    Exclusions, ProfileResolution, effective_profile, merge_profiles, resolve_default,
    resolve_profile,
};
use std::path::Path;

const USAGE: &str = "Usage: mhc doctor

Reports the configuration files that were found and loaded, configuration problems, the active
configuration profile and the model the session-start model profile resolves to.";

/// One report: its lines and the exit code (1 when a check failed).
#[derive(Debug, PartialEq, Eq)]
pub struct DoctorReport {
    pub lines: Vec<String>,
    pub failed: bool,
}

pub fn run_doctor(args: &[String]) -> Result<i32, String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{USAGE}");
        return Ok(0);
    }
    if let Some(unknown) = args.first() {
        maho_core::output_guard::maho_write_stderr(&format!(
            "mhc doctor: unknown argument \"{unknown}\"\n{USAGE}\n"
        ));
        return Ok(2);
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let env = maho_config::process_env();
    let available = available_models()?;
    let report = build_report(&cwd, &env, available.as_deref());
    println!("{}", report.lines.join("\n"));
    Ok(i32::from(report.failed))
}

/// `provider/id` of every model with usable credentials, from `<agent dir>/models.json` and
/// `auth.json`, offline. `None` when the model runtime cannot be created.
pub fn available_models() -> Result<Option<Vec<String>>, String> {
    let agent = std::path::PathBuf::from(maho_core::config::get_agent_dir());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    Ok(runtime.block_on(async {
        let models = maho_core::model_runtime::ModelRuntime::create(
            maho_core::model_runtime::CreateModelRuntimeOptions {
                models_path: Some(agent.join("models.json")),
                auth_path: Some(agent.join("auth.json")),
                agent_dir: Some(agent.clone()),
                allow_model_network: Some(false),
                ..Default::default()
            },
        )
        .await
        .ok()?;
        let available = models.get_available(None).await.ok()?;
        Some(
            available
                .iter()
                .map(|model| format!("{}/{}", model.provider, model.id))
                .collect(),
        )
    }))
}

/// Builds the report for `cwd`. `available` is `None` when the model list could not be read.
pub fn build_report(
    cwd: &Path,
    env: &maho_config::Env,
    available: Option<&[String]>,
) -> DoctorReport {
    let loaded = maho_config::load_native(cwd, env);
    let mut lines = vec!["Configuration".to_owned()];
    if loaded.sources.is_empty() {
        lines.push("  no configuration file locations".to_owned());
    }
    for source in &loaded.sources {
        let state = if source.loaded {
            "loaded"
        } else if source.exists {
            "not loaded"
        } else {
            "not found"
        };
        lines.push(format!(
            "  {:<8}{}  ({state})",
            source.scope.as_str(),
            maho_config::display_path(&source.path, &loaded.home)
        ));
    }
    lines.push(format!(
        "  profile {}",
        loaded.profile.as_deref().unwrap_or("(none)")
    ));
    let problems = maho_config::doctor_lines(&loaded.diagnostics, &loaded.home);
    if problems.is_empty() {
        lines.push("PASS configuration has no problems".to_owned());
    }
    lines.extend(problems.into_iter().map(|line| format!("WARN {line}")));

    lines.push(String::new());
    lines.push("Model profile".to_owned());
    let view = &loaded.view;
    let configured = view
        .get("model_profile")
        .and_then(serde_json::Value::as_str);
    let model_profiles = view.get("model_profiles");
    let profiles = merge_profiles(model_profiles);
    let active = effective_profile(configured, model_profiles);
    lines.push(format!(
        "  active  {}",
        active.as_deref().unwrap_or("(default lanes)")
    ));
    let mut failed = false;
    match available {
        None => lines.push("WARN cannot read the available models".to_owned()),
        Some(available) => {
            lines.push(format!("  models  {} available", available.len()));
            let exclusions = Exclusions::default();
            let result = match &active {
                Some(name) => resolve_profile(&profiles, name, available, &exclusions),
                None => resolve_default(&profiles, available, &exclusions),
            };
            match result {
                ProfileResolution::Resolved {
                    profile,
                    provider,
                    model_id,
                    reasoning,
                    skipped,
                    ..
                } => {
                    let reasoning = reasoning.map(|r| format!(" ({r})")).unwrap_or_default();
                    lines.push(format!(
                        "PASS {profile} resolves to {provider}/{model_id}{reasoning}"
                    ));
                    if !skipped.is_empty() {
                        lines.push(format!("  skipped {}", skipped.join(", ")));
                    }
                }
                ProfileResolution::Unavailable { profile, chain } => lines.push(format!(
                    "WARN {profile}: no model is available (tried {})",
                    chain.join(", ")
                )),
                ProfileResolution::Empty { profile } => {
                    lines.push(format!("WARN {profile}: the profile lists no models"));
                }
                ProfileResolution::Unknown { message, .. } => {
                    failed = true;
                    lines.push(format!("FAIL {message}"));
                }
            }
        }
    }
    DoctorReport { lines, failed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_for(home: &Path) -> maho_config::Env {
        let home = home.to_string_lossy().into_owned();
        [
            ("HOME".to_owned(), home.clone()),
            ("USERPROFILE".to_owned(), home),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn reports_sources_problems_and_unknown_profile() {
        let home = tempfile::tempdir().expect("home");
        let project = home.path().join("work");
        std::fs::create_dir_all(project.join(".omo")).expect("project dir");
        std::fs::create_dir_all(home.path().join(".omo")).expect("user dir");
        std::fs::write(
            home.path().join(".omo/omo.jsonc"),
            r#"{ "model_profile": "no-such-profile" }"#,
        )
        .expect("user config");
        std::fs::write(project.join(".omo/omo.jsonc"), "{ not json").expect("project config");
        let report = build_report(&project, &env_for(home.path()), Some(&[]));
        let text = report.lines.join("\n");
        assert!(text.contains("~/.omo/omo.jsonc  (loaded)"), "{text}");
        assert!(text.contains("(not loaded)"), "{text}");
        assert!(text.contains("WARN config:"), "{text}");
        assert!(
            text.contains("FAIL unknown model profile \"no-such-profile\""),
            "{text}"
        );
        assert!(report.failed);
    }

    #[test]
    fn default_lanes_resolve_against_available_models() {
        let home = tempfile::tempdir().expect("home");
        let profiles = merge_profiles(None);
        let rung = profiles
            .iter()
            .find(|profile| profile.id == maho_model_select::DEFAULT_PROFILE_ORDER[0])
            .and_then(|profile| profile.rungs.iter().find(|rung| !rung.providers.is_empty()))
            .expect("a built-in rung with a provider");
        let available = vec![format!(
            "{}/{}",
            rung.providers[0],
            maho_model_select::model_id_for_provider(&rung.providers[0], &rung.model)
        )];
        let report = build_report(home.path(), &env_for(home.path()), Some(&available));
        let text = report.lines.join("\n");
        assert!(
            text.contains("PASS configuration has no problems"),
            "{text}"
        );
        assert!(text.contains("active  (default lanes)"), "{text}");
        assert!(
            text.contains(&format!("resolves to {}", available[0])),
            "{text}"
        );
        assert!(!report.failed, "{text}");
    }

    #[test]
    fn missing_model_list_is_a_warning() {
        let home = tempfile::tempdir().expect("home");
        let report = build_report(home.path(), &env_for(home.path()), None);
        assert!(
            report
                .lines
                .contains(&"WARN cannot read the available models".to_owned())
        );
        assert!(!report.failed);
    }
}
