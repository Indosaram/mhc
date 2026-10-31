//! Port of senpi `packages/coding-agent/src/cli/agent-dir-command.ts` (v2026.10.27-2).
//!
//! A metadata command, answered before settings, migrations, extensions or hosts. The directory is
//! the very same `get_agent_dir()` every session uses; the remaining fields describe that decision
//! rather than choosing another directory.

use std::collections::HashMap;

use serde_json::json;

/// One versioned JSON record and the exit code for `config agent-dir ...`; `None` for any other argv.
pub fn agent_directory_command_output(
    args: &[String],
    env: &HashMap<String, String>,
    home_dir: &str,
) -> Option<(String, i32)> {
    if args.first().map(String::as_str) != Some("config")
        || args.get(1).map(String::as_str) != Some("agent-dir")
    {
        return None;
    }
    if args.len() != 3 || args[2] != "--json" {
        let record = json!({
            "version": 1,
            "ok": false,
            "code": "invalid_arguments",
            "message": format!("Usage: {} config agent-dir --json", maho_core::config::app_name()),
        });
        return Some((format!("{record}\n"), 2));
    }
    // `get_agent_dir()` panics on the test guard; resolve and check here so the failure is a record,
    // never a panic message on stderr.
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ".".to_owned());
    let env_override = maho_core::brand::env_value("CODING_AGENT_DIR", env);
    let dir = maho_core::config::resolve_agent_dir(&cwd, home_dir, env_override.as_deref());
    if let Err(error) = maho_core::config::assert_agent_dir_allowed_in_tests(
        &dir,
        &maho_core::config::config_dir_name(),
        env,
    ) {
        let record = json!({
            "version": 1,
            "ok": false,
            "code": "selection_failed",
            "message": error.to_string(),
        });
        return Some((format!("{record}\n"), 1));
    }
    Some((format!("{}\n", describe_selection(&dir, env, home_dir)), 0))
}

/// The success record for an already selected `dir`.
pub fn describe_selection(
    dir: &str,
    env: &HashMap<String, String>,
    home_dir: &str,
) -> serde_json::Value {
    let profile = maho_core::brand::brand_profile();
    let selector = maho_core::brand::brand_env_names("CODING_AGENT_DIR", Some(&profile))
        .into_iter()
        .find(|name| env.contains_key(name));
    let explicit = selector.filter(|name| env.get(name).is_some_and(|value| !value.is_empty()));
    let config_dir = maho_core::config::config_dir_name();
    let home_default = if maho_core::config::config_flat_layout() {
        std::path::Path::new(home_dir).join(&config_dir)
    } else {
        std::path::Path::new(home_dir)
            .join(&config_dir)
            .join("agent")
    };
    let rule = if explicit.is_some() {
        "environment"
    } else if std::path::Path::new(dir) == home_default {
        "home-default"
    } else {
        "project-local"
    };
    let source = explicit.unwrap_or_else(|| {
        if rule == "project-local" {
            "project-local".to_owned()
        } else {
            "default".to_owned()
        }
    });
    json!({
        "version": 1,
        "ok": true,
        "dir": dir,
        "source": source,
        "rule": rule,
        "brandedPrefix": profile.env_prefix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn other_commands_are_not_answered() {
        assert!(
            agent_directory_command_output(&argv(&["config"]), &HashMap::new(), "/h").is_none()
        );
        assert!(
            agent_directory_command_output(&argv(&["install", "agent-dir"]), &HashMap::new(), "/h")
                .is_none()
        );
    }

    #[test]
    fn malformed_arguments_are_one_typed_record_with_exit_two() {
        for extra in [
            &[][..],
            &["--help"][..],
            &["--json", "extra"][..],
            &["--json", "--help"][..],
            &["--json", "--json"][..],
            &["--bad", "--json"][..],
        ] {
            let mut args = argv(&["config", "agent-dir"]);
            args.extend(extra.iter().map(|item| (*item).to_owned()));
            let (text, code) =
                agent_directory_command_output(&args, &HashMap::new(), "/h").expect("answered");
            assert_eq!(code, 2);
            assert_eq!(text.trim().lines().count(), 1);
            let record: serde_json::Value = serde_json::from_str(&text).expect("json");
            assert_eq!(record["ok"], false);
            assert_eq!(record["version"], 1);
            assert_eq!(record["code"], "invalid_arguments");
            assert!(record["message"].is_string());
        }
    }

    #[test]
    fn an_explicit_selector_reports_the_environment_rule() {
        let profile = maho_core::brand::brand_profile();
        let name = format!("{}_CODING_AGENT_DIR", profile.env_prefix);
        let env = HashMap::from([(name.clone(), "/store".to_owned())]);
        let record = describe_selection("/store", &env, "/home/u");
        assert_eq!(record["rule"], "environment");
        assert_eq!(record["source"], name);
        assert_eq!(record["brandedPrefix"], profile.env_prefix);
    }

    #[test]
    fn the_home_default_and_project_local_rules() {
        let config_dir = maho_core::config::config_dir_name();
        let home_default = if maho_core::config::config_flat_layout() {
            format!("/home/u/{config_dir}")
        } else {
            format!("/home/u/{config_dir}/agent")
        };
        let record = describe_selection(&home_default, &HashMap::new(), "/home/u");
        assert_eq!(record["rule"], "home-default");
        assert_eq!(record["source"], "default");
        let record = describe_selection("/home/u/p/.x/agent", &HashMap::new(), "/home/u");
        assert_eq!(record["rule"], "project-local");
        assert_eq!(record["source"], "project-local");
    }
}
