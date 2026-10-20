use super::{
    discovery_paths::get_plugins_base_dir,
    install_path_resolver::resolve_actual_install_path,
    installed_plugin_database::{extract_plugin_entries, load_installed_plugins},
    loaded_plugin::create_loaded_plugin,
    plugin_manifest::load_plugin_manifest,
    plugin_settings::{is_plugin_enabled, load_claude_settings},
    scope_filter::should_load_plugin_for_cwd,
    types::*,
};
use crate::shared::logger::log;
use serde_json::Value;
use std::{io, path::Path};

pub fn discover_installed_plugins(options: &PluginLoaderOptions) -> io::Result<PluginLoadResult> {
    let base = options
        .plugins_home_override
        .clone()
        .unwrap_or_else(get_plugins_base_dir);
    let database = load_installed_plugins(Some(&base));
    let settings = match &options.claude_config_dir {
        Some(root) => std::fs::read_to_string(root.join("settings.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok()),
        None => load_claude_settings(),
    };
    let cwd = match &options.cwd {
        Some(cwd) => cwd.clone(),
        None => std::env::current_dir()?,
    };
    Ok(discover_from_database(
        database.as_ref(),
        settings.as_ref(),
        options,
        &cwd,
    ))
}

fn discover_from_database(
    database: Option<&Value>,
    settings: Option<&Value>,
    options: &PluginLoaderOptions,
    cwd: &Path,
) -> PluginLoadResult {
    let mut result = PluginLoadResult::default();
    let Some(database) = database else {
        return result;
    };
    let enabled = settings.and_then(|settings| settings.get("enabledPlugins"));
    let manifest_loader = options
        .load_plugin_manifest_override
        .unwrap_or(load_plugin_manifest);
    for (key, installation) in extract_plugin_entries(database) {
        if !is_plugin_enabled(&key, enabled, options.enabled_plugins_override.as_ref()) {
            log(&format!("Plugin disabled: {key}"), None);
            continue;
        }
        if !should_load_plugin_for_cwd(&installation, cwd) {
            continue;
        }
        let Some(path) = resolve_actual_install_path(&installation.install_path, Some(&key)) else {
            result.errors.push(PluginLoadError {
                plugin_key: key,
                install_path: installation.install_path,
                error: "Plugin installation path does not exist".to_owned(),
            });
            continue;
        };
        let manifest = manifest_loader(&path);
        result
            .plugins
            .push(create_loaded_plugin(&key, &installation, &path, manifest));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{collections::BTreeMap, fs};

    #[test]
    fn discovery_filters_all_database_formats_and_only_uses_first_installation() {
        let root = tempfile::tempdir().expect("fixture directory");
        let project = root.path().join("project");
        let other = root.path().join("other");
        let install = root.path().join("install");
        fs::create_dir(&install).expect("install directory");
        for format in [1, 2, 3] {
            for (scope, path, cwd, expected) in [
                ("project", Some(&project), project.clone(), 1),
                ("project", Some(&project), project.join("packages/app"), 1),
                ("project", Some(&project), other.clone(), 0),
                ("project", None, project.clone(), 0),
                ("local", Some(&project), project.clone(), 1),
                ("local", Some(&project), other.clone(), 0),
                ("user", None, other.clone(), 1),
            ] {
                let entry = json!({"scope":scope,"projectPath":path,"installPath":install,"version":"1.0.0","lastUpdated":"date"});
                let db = match format {
                    1 => json!({"version":1,"plugins":{"demo@market":entry}}),
                    2 => json!({"version":2,"plugins":{"demo@market":[entry]}}),
                    3 => {
                        let mut entry = entry;
                        entry["name"] = json!("demo");
                        entry["marketplace"] = json!("market");
                        json!([entry])
                    }
                    _ => unreachable!("only three fixture formats"),
                };
                let result =
                    discover_from_database(Some(&db), None, &PluginLoaderOptions::default(), &cwd);
                assert_eq!(
                    result.plugins.len(),
                    expected,
                    "format {format}, scope {scope}"
                );
                assert!(result.errors.is_empty());
            }
        }
        let db = json!({"version":2,"plugins":{"multi@market":[
            {"scope":"project","projectPath":other,"installPath":install},
            {"scope":"project","projectPath":project,"installPath":install}
        ]}});
        assert!(
            discover_from_database(Some(&db), None, &PluginLoaderOptions::default(), &project)
                .plugins
                .is_empty()
        );
    }

    #[test]
    fn settings_and_overrides_apply_before_loading_paths() {
        let root = tempfile::tempdir().expect("fixture directory");
        let db = json!({"version":1,"plugins":{"demo@market":{"installPath":root.path(),"version":"1"}}});
        for (settings, override_value, expected) in [
            (json!({"enabledPlugins":{"demo@market":false}}), None, 0),
            (json!({"enabledPlugins":"not-an-object"}), None, 1),
            (
                json!({"enabledPlugins":{"demo@market":false}}),
                Some(true),
                1,
            ),
            (
                json!({"enabledPlugins":{"demo@market":true}}),
                Some(false),
                0,
            ),
        ] {
            let options = PluginLoaderOptions {
                enabled_plugins_override: override_value
                    .map(|value| BTreeMap::from([("demo@market".into(), value)])),
                ..Default::default()
            };
            let result = discover_from_database(Some(&db), Some(&settings), &options, root.path());
            assert_eq!(result.plugins.len(), expected);
            assert!(result.errors.is_empty());
        }
    }

    #[test]
    fn names_components_and_version_fallbacks_match_discovery_contract() {
        let root = tempfile::tempdir().expect("fixture directory");
        for dir in ["commands", "agents", "skills", "hooks"] {
            fs::create_dir(root.path().join(dir)).expect("component directory");
        }
        for file in ["hooks/hooks.json", ".mcp.json"] {
            fs::write(root.path().join(file), "{}").expect("component file");
        }
        for (key, name) in [
            ("@myorg/my-plugin@1.0.0", "@myorg/my-plugin"),
            (
                "file:///D:/configs/node_modules/oh-my-opencode@latest",
                "oh-my-opencode",
            ),
            ("oh-my-openagent@3.13.1", "oh-my-openagent"),
        ] {
            let mut plugins = serde_json::Map::new();
            plugins.insert(
                key.into(),
                json!({"installPath":root.path(),"version":"1.0.0"}),
            );
            let db = json!({"version":1,"plugins":plugins});
            let options = PluginLoaderOptions {
                load_plugin_manifest_override: Some(|_| None),
                ..Default::default()
            };
            let result = discover_from_database(Some(&db), None, &options, root.path());
            let plugin = &result.plugins[0];
            assert_eq!(plugin.name, name);
            assert_eq!(plugin.commands_dir, Some(root.path().join("commands")));
            assert_eq!(plugin.agents_dir, Some(root.path().join("agents")));
            assert_eq!(plugin.skills_dir, Some(root.path().join("skills")));
            assert_eq!(
                plugin.hooks_path,
                Some(root.path().join("hooks/hooks.json"))
            );
            assert_eq!(plugin.mcp_path, Some(root.path().join(".mcp.json")));
        }
        for (installed, manifest, expected) in [
            ("unknown", "2.0.0", "2.0.0"),
            ("", "", "unknown"),
            ("ghost", "2.0.0", "ghost"),
        ] {
            let installation = PluginInstallation {
                version: installed.into(),
                ..Default::default()
            };
            let plugin = create_loaded_plugin(
                "demo@market",
                &installation,
                root.path(),
                Some(PluginManifest {
                    name: "demo".into(),
                    version: Some(manifest.into()),
                    ..Default::default()
                }),
            );
            assert_eq!(plugin.version, expected);
        }
    }

    #[test]
    fn stale_installations_recover_matching_manifests_or_report_original_path() {
        let root = tempfile::tempdir().expect("fixture directory");
        for (directory, manifest) in [
            ("0.1.0", r#"{"name":"demo","version":"0.1.0"}"#),
            (
                "1.2.0-beta.1",
                r#"{"name":"demo","version":"1.2.0-beta.1"}"#,
            ),
            ("1.2.0", r#"{"name":"demo","version":"1.2.0"}"#),
            ("9.0.0", r#"{"name":"other"}"#),
            ("8.0.0", "{ invalid"),
            ("7.0.0", r#"{"version":"7.0.0"}"#),
        ] {
            fs::create_dir(root.path().join(directory)).expect("version directory");
            fs::write(root.path().join(directory).join("plugin.json"), manifest).expect("manifest");
        }
        let configured = root.path().join("unknown");
        let db = json!({"version":2,"plugins":{"demo@market":[{"installPath":configured,"version":"unknown"}]}});
        let result = discover_from_database(
            Some(&db),
            None,
            &PluginLoaderOptions::default(),
            root.path(),
        );
        assert!(result.errors.is_empty());
        assert_eq!(result.plugins[0].install_path, root.path().join("1.2.0"));
        assert_eq!(result.plugins[0].version, "1.2.0");
        for (key, path) in [
            ("missing@market", configured),
            ("demo@market", root.path().join("absent/unknown")),
        ] {
            let mut plugins = serde_json::Map::new();
            plugins.insert(key.into(), json!({"installPath":path}));
            let db = json!({"version":1,"plugins":plugins});
            let result = discover_from_database(
                Some(&db),
                None,
                &PluginLoaderOptions::default(),
                root.path(),
            );
            assert!(result.plugins.is_empty());
            assert_eq!(result.errors[0].install_path, path);
            assert_eq!(
                result.errors[0].error,
                "Plugin installation path does not exist"
            );
        }
        assert_eq!(
            resolve_actual_install_path(&root.path().join("0.1.0"), Some("other@market")),
            Some(root.path().join("0.1.0"))
        );
        let only = tempfile::tempdir().expect("single version fixture");
        fs::create_dir_all(only.path().join("unknown/.claude-plugin")).expect("manifest directory");
        fs::write(
            only.path().join("unknown/.claude-plugin/plugin.json"),
            r#"{"name":"demo","version":"unknown"}"#,
        )
        .expect("manifest");
        assert_eq!(
            resolve_actual_install_path(&only.path().join("ghost"), Some("demo@market")),
            Some(only.path().join("unknown"))
        );
    }
}
