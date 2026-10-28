//! Acceptance tests for Claude Code settings and plugin hook discovery
//! (docs/cleanroom/specs/claude-plugin-hooks.md section 7). Test names carry the spec test number.
#![cfg(feature = "claude-plugins")]
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use maho_ext_hooks::claude_plugins::{ClaudeLocations, derive_plugin_name, discover_plugins, path_contains};
use maho_ext_hooks::config_loader::{HookConfigLoaderOptions, load_hook_config_files_in};
use maho_ext_hooks::types::{HookDiscoveryTiming, HookSourceScope, ParsedHookConfig};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    _root: tempfile::TempDir,
    base: PathBuf,
    locations: ClaudeLocations,
    cwd: PathBuf,
}

impl Fixture {
    fn new() -> std::io::Result<Self> {
        let root = tempfile::tempdir()?;
        let base = std::path::absolute(root.path())?;
        let home = base.join("home");
        let cwd = base.join("project");
        std::fs::create_dir_all(&home)?;
        std::fs::create_dir_all(&cwd)?;
        let locations = ClaudeLocations { claude_dir: base.join("claude"), home };
        Ok(Self { _root: root, base, locations, cwd })
    }

    fn options(&self) -> HookConfigLoaderOptions {
        HookConfigLoaderOptions {
            cwd: self.cwd.to_string_lossy().into_owned(),
            agent_dir: self.base.join("agent").to_string_lossy().into_owned(),
            ..Default::default()
        }
    }

    fn load(&self) -> ParsedHookConfig {
        load_hook_config_files_in(&self.options(), &self.locations)
    }

    fn write_database(&self, database: &Value) -> std::io::Result<()> {
        write_json(&self.locations.plugin_database_path(), database)
    }

    fn write_user_settings(&self, settings: &Value) -> std::io::Result<()> {
        write_json(&self.locations.user_settings_path(), settings)
    }

    /// A plugin install directory with an optional manifest and hooks file.
    fn plugin(&self, relative: &str, manifest: Option<Value>, hooks: Option<Value>) -> std::io::Result<PathBuf> {
        let dir = self.base.join("installs").join(relative);
        std::fs::create_dir_all(&dir)?;
        if let Some(manifest) = manifest {
            write_json(&dir.join(".claude-plugin/plugin.json"), &manifest)?;
        }
        if let Some(hooks) = hooks {
            write_json(&dir.join("hooks/hooks.json"), &hooks)?;
        }
        Ok(dir)
    }
}

fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    std::fs::create_dir_all(path.parent().expect("file has a parent"))?;
    std::fs::write(path, value.to_string())
}

fn pre_tool_hooks(command: &str) -> Value {
    json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": command}]}]}})
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn user_entry(path: &Path) -> Value {
    json!({"scope": "user", "installPath": text(path), "version": "1.0.0"})
}

fn v1(key: &str, entry: Value) -> Value {
    json!({"version": 1, "plugins": {key: entry}})
}

fn env_lookup(pairs: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> {
    let pairs = pairs.iter().map(|(key, value)| ((*key).to_owned(), value.as_os_str().to_owned())).collect::<Vec<_>>();
    move |key| pairs.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone())
}

#[test]
fn t01_missing_claude_dir_adds_nothing() -> TestResult {
    let fixture = Fixture::new()?;
    let parsed = fixture.load();
    assert!(parsed.executable_handlers.is_empty());
    assert!(parsed.diagnostics.is_empty());
    Ok(())
}

#[test]
fn t02_claude_config_dir_overrides_home_location() -> TestResult {
    let fixture = Fixture::new()?;
    let custom = fixture.base.join("custom-claude");
    let home = fixture.locations.home.clone();
    write_json(&custom.join("settings.json"), &pre_tool_hooks("from-custom"))?;
    write_json(&home.join(".claude/settings.json"), &pre_tool_hooks("from-home"))?;

    let set = ClaudeLocations::from_env(env_lookup(&[("CLAUDE_CONFIG_DIR", custom.as_path()), ("HOME", home.as_path())]));
    assert_eq!(set.claude_dir, custom);
    let parsed = load_hook_config_files_in(&fixture.options(), &set);
    assert_eq!(parsed.executable_handlers.len(), 1);
    assert_eq!(parsed.executable_handlers[0].source.scope, HookSourceScope::Global);
    assert_eq!(parsed.executable_handlers[0].config.command, "from-custom");

    for unset in [ClaudeLocations::from_env(env_lookup(&[("HOME", home.as_path())])),
        ClaudeLocations::from_env(env_lookup(&[("CLAUDE_CONFIG_DIR", Path::new("")), ("HOME", home.as_path())])),
        ClaudeLocations::from_env(env_lookup(&[("USERPROFILE", home.as_path())]))] {
        assert_eq!(unset.claude_dir, home.join(".claude"));
        let parsed = load_hook_config_files_in(&fixture.options(), &unset);
        assert_eq!(parsed.executable_handlers.iter().map(|h| h.config.command.as_str()).collect::<Vec<_>>(), ["from-home"]);
    }
    Ok(())
}

#[test]
fn t03_user_then_project_settings_hooks() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write_user_settings(&pre_tool_hooks("user"))?;
    write_json(&fixture.cwd.join(".claude/settings.json"), &json!({"permissions": {}}))?;
    write_json(&fixture.cwd.join(".claude/settings.local.json"), &pre_tool_hooks("local"))?;
    let parsed = fixture.load();
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.executable_handlers.iter().map(|h| (h.source.scope.clone(), h.config.command.as_str())).collect::<Vec<_>>(),
        [(HookSourceScope::Global, "user"), (HookSourceScope::Project, "local")]);
    let orders = parsed.executable_handlers.iter().map(|h| h.source.display_order).collect::<Vec<_>>();
    assert!(orders[0] < orders[1]);
    Ok(())
}

#[test]
fn t04_malformed_project_settings_reports_and_keeps_user_hooks() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.write_user_settings(&pre_tool_hooks("user"))?;
    let broken = fixture.cwd.join(".claude/settings.json");
    std::fs::create_dir_all(fixture.cwd.join(".claude"))?;
    std::fs::write(&broken, "{ not json")?;
    let parsed = fixture.load();
    assert_eq!(parsed.executable_handlers.len(), 1);
    assert_eq!(parsed.executable_handlers[0].config.command, "user");
    assert_eq!(parsed.diagnostics.len(), 1);
    assert_eq!(parsed.diagnostics[0].source.source_path, text(&broken));
    assert!(parsed.diagnostics[0].message.contains(&text(&broken)));
    Ok(())
}

#[test]
fn t05_database_shapes_each_discover_one_plugin() -> TestResult {
    let fixture = Fixture::new()?;
    let install = fixture.plugin("demo", None, None)?;
    let entry = user_entry(&install);
    let mut array_entry = entry.clone();
    array_entry["name"] = json!("demo");
    array_entry["marketplace"] = json!("market");
    for database in [v1("demo@market", entry.clone()),
        json!({"version": 2, "plugins": {"demo@market": [entry.clone()]}}),
        json!({"version": "other", "plugins": {"demo@market": [entry.clone()], "empty@market": [], "bad@market": entry.clone()}}),
        json!([array_entry, {"name": "partial", "installPath": text(&install)}])] {
        fixture.write_database(&database)?;
        let discovery = discover_plugins(&fixture.locations, &fixture.cwd);
        assert_eq!(discovery.plugins.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), ["demo@market"], "{database}");
        assert_eq!(discovery.plugins[0].name, "demo");
        assert_eq!(discovery.plugins[0].version, "1.0.0");
        assert!(discovery.problems.is_empty());
    }
    Ok(())
}

#[test]
fn t06_only_first_v2_install_counts() -> TestResult {
    let fixture = Fixture::new()?;
    let install = fixture.plugin("demo", None, None)?;
    let other = fixture.base.join("elsewhere");
    fixture.write_database(&json!({"version": 2, "plugins": {"demo@market": [
        {"scope": "project", "installPath": text(&install), "projectPath": text(&other)},
        {"scope": "project", "installPath": text(&install), "projectPath": text(&fixture.cwd)},
    ]}}))?;
    assert!(discover_plugins(&fixture.locations, &fixture.cwd).plugins.is_empty());
    Ok(())
}

#[test]
fn t07_project_scope_follows_project_path() -> TestResult {
    let fixture = Fixture::new()?;
    let install = fixture.plugin("demo", None, None)?;
    let project = fixture.base.join("work/a");
    let cases: [(Option<String>, PathBuf, bool); 7] = [
        (Some(text(&project)), project.clone(), true),
        (Some(text(&project)), project.join("sub/dir"), true),
        (Some(text(&project)), fixture.base.join("work/b"), false),
        (Some(text(&project)), fixture.base.join("work"), false),
        (Some(text(&project)), fixture.base.join("work/ab"), false),
        (Some(String::new()), project.clone(), false),
        (None, project.clone(), false),
    ];
    for (project_path, cwd, loads) in cases {
        for scope in ["project", "local"] {
            let mut entry = json!({"scope": scope, "installPath": text(&install)});
            if let Some(path) = &project_path {
                entry["projectPath"] = json!(path);
            }
            fixture.write_database(&v1("demo@market", entry))?;
            assert_eq!(discover_plugins(&fixture.locations, &cwd).plugins.len(), usize::from(loads), "{scope} {project_path:?} {}", cwd.display());
        }
    }
    fixture.write_database(&v1("demo@market", json!({"scope": "project", "installPath": text(&install), "projectPath": "~/work/a"})))?;
    let home_project = fixture.locations.home.join("work/a");
    assert_eq!(discover_plugins(&fixture.locations, &home_project).plugins.len(), 1);
    assert!(discover_plugins(&fixture.locations, &project).plugins.is_empty());
    Ok(())
}

#[test]
fn t08_enabled_plugins_setting() -> TestResult {
    let fixture = Fixture::new()?;
    let install = fixture.plugin("demo", None, None)?;
    fixture.write_database(&v1("demo@market", user_entry(&install)))?;
    for (settings, loads) in [(json!({"enabledPlugins": {"demo@market": false}}), false),
        (json!({"enabledPlugins": {"demo@market": true}}), true),
        (json!({"enabledPlugins": {"other@market": false}}), true),
        (json!({"enabledPlugins": {"demo@market": "no"}}), true),
        (json!({"enabledPlugins": ["demo@market"]}), true)] {
        fixture.write_user_settings(&settings)?;
        assert_eq!(discover_plugins(&fixture.locations, &fixture.cwd).plugins.len(), usize::from(loads), "{settings}");
        assert!(fixture.load().diagnostics.is_empty());
    }
    std::fs::write(fixture.locations.user_settings_path(), "{")?;
    assert_eq!(discover_plugins(&fixture.locations, &fixture.cwd).plugins.len(), 1);
    Ok(())
}

#[test]
fn t09_missing_install_path_picks_best_sibling_version() -> TestResult {
    let fixture = Fixture::new()?;
    let named = |name: &str, version: &str| Some(json!({"name": name, "version": version}));
    fixture.plugin("cache/2.0.0", named("demo", "2.0.0"), None)?;
    fixture.plugin("cache/3.1.0-rc.2", named("demo", "3.1.0-rc.2"), None)?;
    let expected = fixture.plugin("cache/3.1.0", named("demo", "3.1.0"), None)?;
    fixture.plugin("cache/5.0.0", named("other", "5.0.0"), None)?;
    fixture.plugin("cache/unknown", named("demo", "9.9.9"), None)?;
    let malformed = fixture.plugin("cache/4.0.0", None, None)?;
    std::fs::create_dir_all(malformed.join(".claude-plugin"))?;
    std::fs::write(malformed.join(".claude-plugin/plugin.json"), "{")?;
    let missing = fixture.base.join("installs/cache/1.0.0");
    fixture.write_database(&v1("demo@market", json!({"installPath": text(&missing), "version": "unknown"})))?;
    let discovery = discover_plugins(&fixture.locations, &fixture.cwd);
    assert!(discovery.problems.is_empty());
    assert_eq!(discovery.plugins.len(), 1);
    assert_eq!(discovery.plugins[0].install_path, expected);
    assert_eq!(discovery.plugins[0].version, "3.1.0");
    Ok(())
}

#[test]
fn t10_missing_install_path_without_sibling_reports_key() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.plugin("cache/2.0.0", Some(json!({"name": "other"})), None)?;
    let missing = fixture.base.join("installs/cache/1.0.0");
    fixture.write_database(&v1("demo@market", json!({"installPath": text(&missing)})))?;
    let discovery = discover_plugins(&fixture.locations, &fixture.cwd);
    assert!(discovery.plugins.is_empty());
    assert_eq!(discovery.problems.len(), 1);
    assert_eq!(discovery.problems[0].key, "demo@market");
    assert_eq!(discovery.problems[0].install_path, text(&missing));
    let parsed = fixture.load();
    assert!(parsed.executable_handlers.is_empty());
    assert_eq!(parsed.diagnostics.len(), 1);
    assert!(parsed.diagnostics[0].message.contains("demo@market"));
    Ok(())
}

#[test]
fn t11_primary_manifest_wins_over_root_manifest() -> TestResult {
    let fixture = Fixture::new()?;
    let install = fixture.plugin("demo", Some(json!({"name": "primary"})), None)?;
    write_json(&install.join("plugin.json"), &json!({"name": "fallback"}))?;
    fixture.write_database(&v1("demo@market", user_entry(&install)))?;
    assert_eq!(discover_plugins(&fixture.locations, &fixture.cwd).plugins[0].name, "primary");
    std::fs::remove_file(install.join(".claude-plugin/plugin.json"))?;
    assert_eq!(discover_plugins(&fixture.locations, &fixture.cwd).plugins[0].name, "fallback");
    Ok(())
}

#[test]
fn t12_name_derivation() -> TestResult {
    assert_eq!(derive_plugin_name("demo@market"), "demo");
    assert_eq!(derive_plugin_name("@acme/kit@2.3.4"), "@acme/kit");
    assert_eq!(derive_plugin_name("file:///C:/plugins/lib/widget@next"), "widget");
    assert_eq!(derive_plugin_name("npm:@acme/kit"), "@acme/kit");
    assert_eq!(derive_plugin_name("npm:tool@1.0.0"), "tool");
    assert_eq!(derive_plugin_name("C:\\plugins\\gadget@local"), "gadget");

    let fixture = Fixture::new()?;
    let install = fixture.plugin("unnamed", Some(json!({"name": "", "version": "2.0.0"})), None)?;
    fixture.write_database(&v1("@acme/kit@2.3.4", json!({"installPath": text(&install)})))?;
    let discovery = discover_plugins(&fixture.locations, &fixture.cwd);
    assert_eq!(discovery.plugins[0].name, "@acme/kit");
    assert_eq!(discovery.plugins[0].version, "2.0.0");
    Ok(())
}

#[test]
fn t13_plugin_hooks_substitute_root_and_record_plugin_source() -> TestResult {
    let fixture = Fixture::new()?;
    let install = fixture.plugin("demo", Some(json!({"name": "demo"})), Some(pre_tool_hooks("${CLAUDE_PLUGIN_ROOT}/check")))?;
    std::fs::write(install.join("check"), "")?;
    fixture.write_database(&v1("demo@market", user_entry(&install)))?;
    let parsed = fixture.load();
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.executable_handlers.len(), 1);
    let handler = &parsed.executable_handlers[0];
    assert_eq!(handler.source.scope, HookSourceScope::Plugin);
    assert_eq!(handler.config.command, format!("{}/check", install.display()));
    assert_eq!(handler.source.plugin_root.as_deref(), Some(text(&install).as_str()));
    assert_eq!(handler.source.discovered_at, HookDiscoveryTiming::PreSession);
    assert_eq!(handler.source.manifest_path, Some(text(&install.join(".claude-plugin/plugin.json"))));
    assert_eq!(handler.source.source_path, text(&install.join("hooks/hooks.json")));
    Ok(())
}

#[test]
fn t14_placeholders_replaced_everywhere_and_other_handler_types_untagged() -> TestResult {
    let mut nested = json!({"a": "${CLAUDE_PLUGIN_ROOT}:${CLAUDE_PLUGIN_ROOT}", "b": [{"c": ["x ${CLAUDE_PLUGIN_ROOT} y"]}], "d": 3});
    maho_ext_hooks::claude_plugins::substitute_plugin_root(&mut nested, "/root");
    assert_eq!(nested, json!({"a": "/root:/root", "b": [{"c": ["x /root y"]}], "d": 3}));

    let fixture = Fixture::new()?;
    let hooks = json!({"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
        {"type": "command", "command": "echo ${CLAUDE_PLUGIN_ROOT} ${CLAUDE_PLUGIN_ROOT}", "statusMessage": "in ${CLAUDE_PLUGIN_ROOT}"},
        {"type": "prompt", "prompt": "check ${CLAUDE_PLUGIN_ROOT}"},
        {"type": "agent", "prompt": "review"},
    ]}]}});
    let install = fixture.plugin("demo", None, Some(hooks))?;
    fixture.write_database(&v1("demo@market", user_entry(&install)))?;
    let parsed = fixture.load();
    let root = text(&install);
    assert_eq!(parsed.executable_handlers.len(), 1);
    let command = &parsed.executable_handlers[0];
    assert_eq!(command.config.command, format!("echo {root} {root}"));
    assert_eq!(command.config.status_message, Some(format!("in {root}")));
    assert_eq!(command.source.plugin_root.as_deref(), Some(root.as_str()));
    assert!(parsed.executable_handlers.iter().all(|handler| handler.config.kind == "command"));
    Ok(())
}

#[test]
fn t15_malformed_plugin_hooks_file_skips_only_that_plugin() -> TestResult {
    let fixture = Fixture::new()?;
    let broken = fixture.plugin("broken", None, None)?;
    std::fs::create_dir_all(broken.join("hooks"))?;
    std::fs::write(broken.join("hooks/hooks.json"), "{ nope")?;
    let good = fixture.plugin("good", None, Some(pre_tool_hooks("good")))?;
    fixture.write_database(&json!({"version": 1, "plugins": {"broken@market": user_entry(&broken), "good@market": user_entry(&good)}}))?;
    let parsed = fixture.load();
    assert_eq!(parsed.executable_handlers.iter().map(|h| h.config.command.as_str()).collect::<Vec<_>>(), ["good"]);
    Ok(())
}

#[test]
fn t16_explicit_plugin_source_registers_once() -> TestResult {
    let fixture = Fixture::new()?;
    let install = fixture.plugin("demo", None, Some(pre_tool_hooks("once")))?;
    fixture.write_database(&v1("demo@market", user_entry(&install)))?;
    let mut options = fixture.options();
    options.pre_session_hook_source_paths = vec![text(&install.join("hooks/hooks.json"))];
    let parsed = load_hook_config_files_in(&options, &fixture.locations);
    assert_eq!(parsed.executable_handlers.len(), 1);
    assert_eq!(parsed.executable_handlers[0].config.command, "once");
    Ok(())
}

#[test]
fn source_order_mhc_then_user_project_local_then_plugins() -> TestResult {
    let fixture = Fixture::new()?;
    let own = fixture.base.join("agent/hooks.json");
    write_json(&own, &pre_tool_hooks("own"))?;
    fixture.write_user_settings(&pre_tool_hooks("user"))?;
    write_json(&fixture.cwd.join(".claude/settings.json"), &pre_tool_hooks("project"))?;
    write_json(&fixture.cwd.join(".claude/settings.local.json"), &pre_tool_hooks("local"))?;
    let first = fixture.plugin("first", None, Some(pre_tool_hooks("plugin-1")))?;
    let second = fixture.plugin("second", None, Some(pre_tool_hooks("plugin-2")))?;
    fixture.write_database(&json!({"version": 1, "plugins": {"b@m": user_entry(&first), "a@m": user_entry(&second)}}))?;
    let parsed = fixture.load();
    assert_eq!(parsed.executable_handlers.iter().map(|h| h.config.command.as_str()).collect::<Vec<_>>(), ["own", "user", "project", "local", "plugin-1", "plugin-2"]);
    let orders = parsed.executable_handlers.iter().map(|h| h.source.display_order).collect::<Vec<_>>();
    assert!(orders.windows(2).all(|pair| pair[1] == pair[0] + 1), "{orders:?}");
    Ok(())
}

#[test]
fn path_containment_compares_whole_components() -> TestResult {
    let root = tempfile::tempdir()?;
    let base = root.path();
    assert!(path_contains(&base.join("a/proj"), &base.join("a/proj")));
    assert!(path_contains(&base.join("a/proj"), &base.join("a/proj/x")));
    assert!(!path_contains(&base.join("a/proj"), &base.join("a/project")));
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_backslash_home_expansion() -> TestResult {
    let fixture = Fixture::new()?;
    let install = fixture.plugin("demo", None, None)?;
    fixture.write_database(&v1("demo@market", json!({"scope": "local", "installPath": text(&install), "projectPath": "~\\work\\a"})))?;
    assert_eq!(discover_plugins(&fixture.locations, &fixture.locations.home.join("work").join("a")).plugins.len(), 1);
    Ok(())
}

#[cfg(unix)]
mod end_to_end {
    use super::*;
    use maho_ext_api::types::ToolCallEvent;
    use maho_ext_hooks::command_runner::{CommandHookRunOptions, run_command_hook};
    use maho_ext_hooks::dispatcher::{HookDispatchDecision, HookDispatchResult, dispatch_hook_event};
    use maho_ext_hooks::tool_adapter::{apply_pre_tool_use_result, build_pre_tool_use_hook_input};
    use maho_ext_hooks::types::{ExecutableHookHandler, HookTrustState};

    fn trusted(handlers: &[ExecutableHookHandler]) -> Result<HookTrustState, Box<dyn std::error::Error>> {
        let mut state = maho_ext_hooks::trust_state_json::empty_hook_trust_state();
        for handler in handlers {
            state.hooks.insert(maho_ext_hooks::trust::hook_trust_id(handler), maho_ext_hooks::trust::create_hook_trust_entry(handler, "linux", "fixed")?);
        }
        Ok(state)
    }

    async fn call_tool(handlers: &[ExecutableHookHandler], trust: &HookTrustState, tool: &str, cwd: &Path) -> Result<(ToolCallEvent, HookDispatchResult), Box<dyn std::error::Error>> {
        let event = ToolCallEvent { tool_call_id: "call-1".to_owned(), tool_name: tool.to_owned(), input: json!({"command": "ls"}) };
        let input = build_pre_tool_use_hook_input(&event, &text(cwd), "session-1");
        let cwd = cwd.to_owned();
        let wire = input.clone();
        let result = dispatch_hook_event(handlers, &input, trust, "linux", move |handler| {
            let cwd = cwd.clone();
            let wire = wire.clone();
            async move {
                run_command_hook(&handler, &wire, CommandHookRunOptions { cwd: &cwd, env_passthrough: &[], output_policy: None, signal: None, source_env: None }).await
            }
        })
        .await?;
        Ok((event, result))
    }

    fn blocking_plugin(fixture: &Fixture) -> std::io::Result<Vec<ExecutableHookHandler>> {
        let install = fixture.plugin("demo", None, Some(pre_tool_hooks("printf blocked >&2; exit 2")))?;
        fixture.write_database(&v1("demo@market", user_entry(&install)))?;
        Ok(fixture.load().executable_handlers)
    }

    #[tokio::test]
    async fn t17_trusted_plugin_hook_blocks_bash_only() -> TestResult {
        let fixture = Fixture::new()?;
        let handlers = blocking_plugin(&fixture)?;
        assert_eq!(handlers.len(), 1);
        assert_eq!(maho_ext_hooks::trust::hook_trust_storage_scope(&handlers[0], false), Some(maho_ext_hooks::trust::HookTrustStorageScope::Global));
        let trust = trusted(&handlers)?;

        let (mut bash, result) = call_tool(&handlers, &trust, "bash", &fixture.cwd).await?;
        assert!(matches!(&result.decision, HookDispatchDecision::Block { reason: Some(reason), .. } if reason == "blocked"), "{:?}", result.decision);
        assert_eq!(result.summaries[0].run.stderr, "blocked");
        let refused = apply_pre_tool_use_result(&mut bash, &result).ok_or("bash call was not refused")?;
        assert_eq!(refused.block, Some(true));

        let (mut read, result) = call_tool(&handlers, &trust, "read", &fixture.cwd).await?;
        assert!(result.summaries.is_empty());
        assert_eq!(result.decision, HookDispatchDecision::None);
        assert!(apply_pre_tool_use_result(&mut read, &result).is_none());
        Ok(())
    }

    #[tokio::test]
    async fn t18_untrusted_plugin_hook_does_not_run() -> TestResult {
        let fixture = Fixture::new()?;
        let handlers = blocking_plugin(&fixture)?;
        let trust = maho_ext_hooks::trust_state_json::empty_hook_trust_state();
        let (mut bash, result) = call_tool(&handlers, &trust, "bash", &fixture.cwd).await?;
        assert!(result.summaries.is_empty());
        assert_eq!(result.skipped.len(), 1);
        assert_eq!(result.skipped[0].reason, "untrusted");
        assert!(apply_pre_tool_use_result(&mut bash, &result).is_none());
        Ok(())
    }

    #[tokio::test]
    async fn t19_hook_process_sees_plugin_root_variable() -> TestResult {
        let fixture = Fixture::new()?;
        let install = fixture.plugin("demo", None, Some(pre_tool_hooks("printf '%s|%s' \"$CLAUDE_PLUGIN_ROOT\" \"$CLAUDE_PLUGIN_DATA\"")))?;
        fixture.write_database(&v1("demo@market", user_entry(&install)))?;
        let handlers = fixture.load().executable_handlers;
        let trust = trusted(&handlers)?;
        let (_, result) = call_tool(&handlers, &trust, "bash", &fixture.cwd).await?;
        let stdout = &result.summaries[0].run.stdout;
        let (root, data) = stdout.split_once('|').ok_or("missing separator")?;
        assert_eq!(root, text(&install));
        assert!(!data.is_empty());
        Ok(())
    }
}

/// Test 20: the feature links nothing under `crates/omo/`; the `mit-profile` CI job builds
/// maho-cli with `--no-default-features` (which keeps this feature on) and runs this suite.
#[test]
fn t20_manifest_has_no_omo_dependency() -> TestResult {
    let manifest = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))?;
    assert!(!manifest.contains("/omo/"));
    assert!(!manifest.contains("claude-code-compat-core"));
    Ok(())
}
