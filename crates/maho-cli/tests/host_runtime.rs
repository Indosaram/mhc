use maho_cli::cli::args::Args;
use maho_cli::cli::host_runtime::{mount_agent_session_runtime, resolve_cli_initial_model, resolve_cli_path, resolve_runtime_project_trust, CliRuntimeConfiguration, CliRuntimeRequest};
use maho_core::project_trust::AppMode;

#[tokio::test]
async fn fresh_session_reports_the_origin_its_model_came_from() {
    let dir = tempfile::tempdir().expect("isolated model runtime");
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(&agent).expect("agent dir");
    let cwd = dir.path().to_string_lossy();
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    }).expect("valid model runtime");
    let settings = maho_core::settings_manager::SettingsManager::create(&cwd, &agent.to_string_lossy(), &cwd, false);
    let pinned = resolve_cli_initial_model(
        Some(("faux".to_owned(), "faux-1".to_owned())), None, false, &settings, &runtime, &[]).await.expect("initial model");
    assert_eq!(pinned.provenance, Some("cli"));
    let automatic = resolve_cli_initial_model(None, None, false, &settings, &runtime, &[]).await.expect("initial model");
    assert_eq!(automatic.provenance, Some("first-available"));
    let continuing = resolve_cli_initial_model(None, None, true, &settings, &runtime, &[]).await.expect("initial model");
    assert_eq!(continuing.provenance, None);
    assert!(continuing.model.is_none());
}

fn scoped_fixture() -> (
    tempfile::TempDir,
    maho_ai::providers::faux::FauxProviderHandle,
    maho_core::model_runtime::ModelRuntime,
    maho_core::settings_manager::SettingsManager,
) {
    let dir = tempfile::tempdir().expect("isolated model runtime");
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(&agent).expect("agent dir");
    let cwd = dir.path().to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(maho_ai::providers::faux::RegisterFauxProviderOptions {
        models: Some(["qa-alpha", "qa-beta"].map(|id| maho_ai::providers::faux::FauxModelDefinition {
            id: id.to_owned(), reasoning: Some(true), ..Default::default()
        }).to_vec()),
        ..Default::default()
    }).expect("valid faux provider");
    std::fs::write(agent.join("settings.json"), "{\"defaultProvider\":\"faux\",\"defaultModel\":\"qa-beta\"}\n")
        .expect("settings fixture");
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(dir.path().join("models.json")), auth_path: Some(dir.path().join("auth.json")),
        providers: Some(Vec::new()), ..Default::default()
    }).expect("valid model runtime");
    runtime.register_native_provider(provider.provider.clone());
    runtime.register_provider("faux", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider { api_key: Some("fixture-key".into()), ..Default::default() },
        ..Default::default()
    }).expect("configured provider");
    let catalog = runtime.get_models(None);
    for id in ["qa-alpha", "qa-beta"] {
        assert!(catalog.iter().any(|model| model.provider == "faux" && model.id == id), "the fixture registers {id}");
    }
    let settings = maho_core::settings_manager::SettingsManager::create(&cwd, &agent.to_string_lossy(), &cwd, false);
    assert_eq!(settings.get_string("defaultModel").as_deref(), Some("qa-beta"));
    (dir, provider, runtime, settings)
}

#[tokio::test]
async fn a_scoped_selection_outranks_the_settings_default_and_reports_the_scoped_origin() {
    let (_dir, provider, runtime, settings) = scoped_fixture();
    let scope = maho_core::model_resolver::resolve_model_scope_from_models(&["qa-alpha:high".to_owned()], &provider.models);
    assert_eq!(scope.scoped_models.len(), 1);
    assert_eq!(scope.scoped_models[0].model.id, "qa-alpha");
    let entries = maho_cli::cli::startup::session_model_entries(scope.scoped_models.clone()).expect("scoped entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].model.id, "qa-alpha");
    assert_eq!(entries[0].thinking_level, Some(maho_ai::types::ThinkingLevel::High));
    let resolved = resolve_cli_initial_model(None, None, false, &settings, &runtime, &scope.scoped_models).await.expect("initial model");
    assert_eq!(resolved.provenance, Some("scoped"));
    assert_eq!(resolved.model.as_ref().map(|model| model.id.as_str()), Some("qa-alpha"));
    assert_ne!(resolved.model.as_ref().map(|model| model.id.as_str()), settings.get_string("defaultModel").as_deref());
    assert_eq!(resolved.thinking_level, Some(maho_ai::types::ModelThinkingLevel::High));
    assert_eq!(resolved.thinking_selection.map(|selection| selection.level), Some(maho_ai::types::ModelThinkingLevel::High));
}

#[tokio::test]
async fn an_explicit_pair_outranks_a_scoped_selection() {
    let (_dir, provider, runtime, settings) = scoped_fixture();
    let scope = maho_core::model_resolver::resolve_model_scope_from_models(&["qa-alpha:high".to_owned()], &provider.models);
    let explicit = resolve_cli_initial_model(
        Some(("faux".to_owned(), "qa-beta".to_owned())), None, false, &settings, &runtime, &scope.scoped_models).await.expect("initial model");
    assert_eq!(explicit.provenance, Some("cli"));
    assert_eq!(explicit.model.as_ref().map(|model| model.id.as_str()), Some("qa-beta"));
    assert_ne!(explicit.model.as_ref().map(|model| model.id.as_str()), scope.scoped_models.first().map(|scoped| scoped.model.id.as_str()));
}

#[tokio::test]
async fn a_resumed_session_resolves_no_model_and_keeps_no_origin() {
    let (_dir, provider, runtime, settings) = scoped_fixture();
    let scope = maho_core::model_resolver::resolve_model_scope_from_models(&["qa-alpha:high".to_owned()], &provider.models);
    let resumed = resolve_cli_initial_model(None, None, true, &settings, &runtime, &scope.scoped_models).await.expect("initial model");
    assert_eq!(resumed.provenance, None);
    assert!(resumed.model.is_none());
    assert!(resumed.thinking_level.is_none());
    assert!(resumed.thinking_selection.is_none());
}

#[tokio::test]
async fn initial_model_reports_availability_errors_instead_of_no_model() {
    let (dir, _provider, runtime, _) = scoped_fixture();
    runtime.credentials.set("faux", Some(serde_json::json!({"type":"invalid-fixture"}))).expect("invalid stored credential");
    let cwd = dir.path().to_string_lossy();
    let settings = maho_core::settings_manager::SettingsManager::create(&cwd, &dir.path().join("empty-agent").to_string_lossy(), &cwd, false);
    let result = resolve_cli_initial_model(None, None, false, &settings, &runtime, &[]).await;
    assert!(result.is_err());
    assert!(runtime.get_error().is_some());
}

fn config(parsed: &Args) -> CliRuntimeConfiguration {
    let project = if cfg!(windows) { "C:\\tmp\\project" } else { "/tmp/project" };
    let agent = if cfg!(windows) { "C:\\tmp\\agent" } else { "/tmp/agent" };
    CliRuntimeConfiguration::from_parsed(parsed, project, agent, AppMode::Interactive)
}

#[tokio::test]
async fn a_mounted_session_is_bound_to_the_per_runtime_cwd() {
    let dir = tempfile::tempdir().expect("isolated mount");
    let launch = dir.path().join("launch");
    let project = dir.path().join("project");
    let agent = dir.path().join("agent");
    for path in [&launch, &project, &agent] { std::fs::create_dir_all(path).expect("fixture directory"); }
    let launch_text = launch.to_string_lossy().into_owned();
    let project_text = project.to_string_lossy().into_owned();
    let provider = maho_ai::providers::faux::faux_provider(Default::default()).expect("valid faux provider");
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(agent.join("models.json")), auth_path: Some(agent.join("auth.json")),
        providers: Some(Vec::new()), ..Default::default()
    }).expect("valid model runtime");
    runtime.register_native_provider(provider.provider.clone());
    runtime.register_provider("faux", maho_core::provider_composer::ProviderConfigInput {
        config: maho_core::model_config_schema::ModelsJsonProvider { api_key: Some("fixture-key".into()), ..Default::default() },
        ..Default::default()
    }).expect("configured provider");
    let model = provider.get_model(Some("faux-1")).expect("fixture model");
    let config = CliRuntimeConfiguration {
        cwd: launch_text.clone(), agent_dir: agent.to_string_lossy().into_owned(),
        app_mode: AppMode::Rpc, ..Default::default()
    };
    let request = CliRuntimeRequest { cwd: Some(project_text.clone()), model: Some(model), ..Default::default() };
    let mounted = mount_agent_session_runtime(&config, request, Some(runtime), None).await.expect("the runtime mounts");
    assert_eq!(mounted.runtime.cwd(), project_text.as_str());
    assert_eq!(mounted.session().cwd(), project_text.as_str());
    assert_ne!(mounted.session().cwd(), launch_text.as_str());
    mounted.session().dispose().await;
}

#[test]
fn launch_profile_maps_provider_model_thinking_and_auto_title() {
    let parsed = Args {
        provider: Some("openai".to_owned()),
        model: Some("gpt-4o".to_owned()),
        thinking: Some("high".to_owned()),
        auto_title_sessions: true,
        ..Default::default()
    };
    let profile = config(&parsed).launch_profile();
    assert_eq!(profile.cwd, if cfg!(windows) { "C:\\tmp\\project" } else { "/tmp/project" });
    assert_eq!(profile.creation_model, Some(("openai".to_owned(), "gpt-4o".to_owned())));
    assert_eq!(profile.initial_thinking_level.as_deref(), Some("high"));
    assert_eq!(profile.auto_title, Some(true));
    assert_eq!(profile.permission_preset, None);
}

#[test]
fn launch_profile_omits_a_partial_creation_model() {
    let parsed = Args { provider: Some("openai".to_owned()), ..Default::default() };
    let profile = config(&parsed).launch_profile();
    assert_eq!(profile.creation_model, None);
    assert_eq!(profile.auto_title, None);
}

#[test]
fn metadata_launches_resolve_trust_as_a_print_run() {
    for parsed in [
        Args { help: true, ..Default::default() },
        Args { list_tips: true, ..Default::default() },
        Args { list_models: Some(String::new()), ..Default::default() },
    ] {
        assert_eq!(config(&parsed).trust_prompt_mode(), AppMode::Print);
    }
    assert_eq!(config(&Args::default()).trust_prompt_mode(), AppMode::Interactive);
}

#[test]
fn cli_paths_resolve_against_the_launch_cwd() {
    let (w, abs_x, rel_x_expected, abs_f, abs_tpl) = if cfg!(windows) {
        ("C:\\w", "C:\\abs\\x", "C:\\w\\rel\\x", "file:///C:/abs/f", "C:\\abs\\templates")
    } else {
        ("/w", "/abs/x", "/w/rel/x", "file:///abs/f", "/abs/templates")
    };
    assert_eq!(resolve_cli_path(w, abs_x), abs_x);
    assert_eq!(resolve_cli_path(w, "rel/x"), rel_x_expected);
    assert_eq!(resolve_cli_path(w, "npm:@x/y"), "npm:@x/y");
    assert_eq!(resolve_cli_path(w, "git:github.com/a/b"), "git:github.com/a/b");
    assert_eq!(resolve_cli_path(w, abs_f), if cfg!(windows) { "C:\\abs\\f" } else { "/abs/f" });
    let parsed = Args {
        skills: vec!["skills".to_owned()],
        prompt_templates: vec![abs_tpl.to_owned()],
        themes: vec!["themes".to_owned()],
        extensions: vec!["ext".to_owned()],
        ..Default::default()
    };
    let config = config(&parsed);
    let project = if cfg!(windows) { "C:\\tmp\\project" } else { "/tmp/project" };
    assert_eq!(config.resolved_skill_paths(), vec![std::path::Path::new(project).join("skills").to_string_lossy().into_owned()]);
    assert_eq!(config.resolved_prompt_template_paths(), vec![abs_tpl.to_owned()]);
    assert_eq!(config.resolved_theme_paths(), vec![std::path::Path::new(project).join("themes").to_string_lossy().into_owned()]);
    assert_eq!(config.resolved_extension_paths(), vec![std::path::Path::new(project).join("ext").to_string_lossy().into_owned()]);
}

#[test]
fn configuration_carries_the_parsed_session_directory() {
    let parsed = Args { session_dir: Some("/sessions".to_owned()), ..Default::default() };
    assert_eq!(config(&parsed).session_dir.as_deref(), Some("/sessions"));
}

fn trust_fixture() -> (tempfile::TempDir, String, String) {
    let dir = tempfile::tempdir().expect("trust fixture");
    let project = dir.path().join("project");
    std::fs::create_dir_all(project.join(".omo")).expect("project config dir");
    std::fs::write(project.join(".omo/settings.json"), "{}").expect("project settings");
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(&agent).expect("agent dir");
    let project = project.to_string_lossy().into_owned();
    let agent = agent.to_string_lossy().into_owned();
    (dir, project, agent)
}

#[tokio::test]
async fn the_approve_override_trusts_the_runtime_project() {
    let (_dir, project, agent) = trust_fixture();
    let config = CliRuntimeConfiguration { cwd: project.clone(), agent_dir: agent, project_trust_override: Some(true), ..Default::default() };
    assert!(resolve_runtime_project_trust(&config, &project, AppMode::Print, false, Box::new(|_, _| None)).await);
}

#[tokio::test]
async fn a_saved_trust_decision_is_honoured_and_cached_per_cwd() {
    let (_dir, project, agent) = trust_fixture();
    maho_core::trust_manager::ProjectTrustStore::new(&agent).set(&project, Some(true)).expect("saved decision");
    let config = CliRuntimeConfiguration { cwd: project.clone(), agent_dir: agent, ..Default::default() };
    assert!(resolve_runtime_project_trust(&config, &project, AppMode::Print, false, Box::new(|_, _| None)).await);
    assert_eq!(config.project_trust_by_cwd.lock().expect("cache").get(&project).copied(), Some(true));
}

#[tokio::test]
async fn an_unsaved_project_without_ui_stays_untrusted() {
    let (_dir, project, agent) = trust_fixture();
    let config = CliRuntimeConfiguration { cwd: project.clone(), agent_dir: agent, ..Default::default() };
    assert!(!resolve_runtime_project_trust(&config, &project, AppMode::Print, false, Box::new(|_, _| None)).await);
}
/// Supervisor session-policy probes (`maho_cli::cli::session_policy`); the policy is enforced
/// only by the omo build, so the MIT profile compiles none of them.
#[cfg(feature = "omo")]
mod supervisor_policy {
    use maho_cli::cli::host_runtime::{mount_agent_session_runtime, CliRuntimeConfiguration, CliRuntimeRequest};
    use maho_core::project_trust::AppMode;

    const SUPERVISOR_TOOLS: [&str; 2] = ["supervisor_submit_work", "supervisor_work_status"];
    const MEMORY_WRITE_TOOLS: [&str; 2] = ["memory", "memory_apply_patch"];

    /// Mounts one runtime the way the shared host does for `open_session` with `context`.
    async fn mount_with_context(root: &std::path::Path, context: &[(&str, &str)], session_manager: Option<maho_core::session_manager::SessionManager>)
        -> Result<maho_cli::cli::host_runtime::MountedRuntime, String> {
        let project = root.join("project");
        let agent = root.join("agent");
        for path in [&project, &agent] { std::fs::create_dir_all(path).expect("fixture directory"); }
        let provider = maho_ai::providers::faux::faux_provider(Default::default()).expect("valid faux provider");
        let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(agent.join("models.json")), auth_path: Some(agent.join("auth.json")),
            providers: Some(Vec::new()), ..Default::default()
        }).expect("valid model runtime");
        runtime.register_native_provider(provider.provider.clone());
        runtime.register_provider("faux", maho_core::provider_composer::ProviderConfigInput {
            config: maho_core::model_config_schema::ModelsJsonProvider { api_key: Some("fixture-key".into()), ..Default::default() },
            ..Default::default()
        }).expect("configured provider");
        let project = project.to_string_lossy().into_owned();
        let config = CliRuntimeConfiguration { cwd: project.clone(), agent_dir: agent.to_string_lossy().into_owned(), app_mode: AppMode::Rpc, ..Default::default() };
        let request = CliRuntimeRequest {
            cwd: Some(project), model: provider.get_model(Some("faux-1")), session_manager,
            session_profile: maho_ext_api::ExtensionSessionProfile {
                session_context: context.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        mount_agent_session_runtime(&config, request, Some(runtime), None).await
    }

    /// The registered tool names; MCP tools register when their servers connect, so they are left out.
    fn registered_tools(mounted: &maho_cli::cli::host_runtime::MountedRuntime) -> std::collections::BTreeSet<String> {
        mounted.session().get_all_tools().into_iter().map(|tool| tool.name).filter(|name| !name.starts_with("mcp_")).collect()
    }

    /// Root of the isolated fixture a probe runs in; set only by [`run_isolated`].
    const PROBE_ROOT_ENV: &str = "MAHO_HOST_RUNTIME_PROBE_ROOT";

    /// The mount reads the process env and cwd (HOME, `~/.omo` config, the memory root, agent dir), so
    /// each policy probe runs as an `#[ignore]` test of this binary in a child process with a cleared
    /// environment, an isolated HOME whose `~/.omo/omo.json` pins memory on, and the fixture project as
    /// cwd. Results never depend on the developer's own configuration.
    fn run_isolated(probe: &str, agent_settings: Option<&str>) {
        let dir = tempfile::tempdir().expect("isolated root");
        let home = dir.path().join("home");
        for path in [home.join(".omo"), dir.path().join("project"), dir.path().join("agent")] { std::fs::create_dir_all(path).expect("fixture directory"); }
        std::fs::write(home.join(".omo/omo.json"), "{\"memory\":{\"enabled\":true}}\n").expect("pinned memory config");
        if let Some(settings) = agent_settings { std::fs::write(dir.path().join("agent/settings.json"), settings).expect("agent settings"); }
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        command.args([format!("{}::{probe}", module_path!().split_once("::").map_or("", |(_, path)| path)).as_str(), "--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env_clear().env("HOME", &home).env("PATH", std::env::var_os("PATH").unwrap_or_default()).env(PROBE_ROOT_ENV, dir.path())
            .current_dir(dir.path().join("project"));
        if cfg!(windows) {
            // Windows resolves the home and temp directories from these, not HOME; a child process
            // also needs SystemRoot. All point into the fixture, never at the real profile.
            let (roaming, local, temp) = (home.join("AppData").join("Roaming"), home.join("AppData").join("Local"), dir.path().join("temp"));
            for path in [&roaming, &local, &temp] { std::fs::create_dir_all(path).expect("fixture profile directory"); }
            if let Some(system_root) = std::env::var_os("SystemRoot") { command.env("SystemRoot", system_root); }
            // Bash resolution finds Git Bash under these (`maho_tools::shell::get_shell_config`); a
            // Windows host always defines them, so the cleared env must not hide the shell.
            for key in ["ProgramFiles", "ProgramFiles(x86)"] {
                if let Some(value) = std::env::var_os(key) { command.env(key, value); }
            }
            command.env("USERPROFILE", &home).env("APPDATA", &roaming).env("LOCALAPPDATA", &local).env("TEMP", &temp).env("TMP", &temp);
        }
        let output = command.output().expect("probe process");
        let (stdout, stderr) = (String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "probe {probe} failed:\n{stdout}\n{stderr}");
        assert!(stdout.contains("1 passed"), "probe {probe} did not run exactly once:\n{stdout}");
    }

    fn probe_root() -> std::path::PathBuf {
        std::path::PathBuf::from(std::env::var(PROBE_ROOT_ENV).expect("policy probes run only through run_isolated"))
    }

    #[test]
    fn supervisor_tools_memory_write_deny_allowed_set_is_exact() { run_isolated("probe_memory_write_deny_allowed_set", None); }
    #[test]
    fn supervisor_tools_reject_unknown_and_malformed_policy_with_typed_errors() { run_isolated("probe_reject_unknown_and_malformed_policy", None); }
    #[test]
    fn supervisor_tools_policy_persists_across_resume() { run_isolated("probe_policy_persists_across_resume", None); }
    #[test]
    fn supervisor_tools_policy_survives_session_replacement() { run_isolated("probe_policy_survives_session_replacement", None); }
    #[test]
    fn supervisor_tools_fail_the_open_when_default_tools_filter_them() { run_isolated("probe_work_tools_filtered_by_default_tools", Some("{\"defaultTools\":[\"read\",\"bash\"]}\n")); }
    #[test]
    fn supervisor_tools_denied_session_refuses_prompted_commands_and_memory_file_writes() { run_isolated("probe_denied_prompted_commands_and_file_writes", None); }
    #[test]
    fn supervisor_tools_cross_role_switch_is_a_typed_error() { run_isolated("probe_cross_role_switch_rejected", None); }
    #[test]
    fn supervisor_tools_memory_write_deny_fails_the_open_without_a_memory_bind() { run_isolated("probe_deny_without_memory_bind_fails_the_open", None); }
    #[test]
    fn supervisor_tools_effect_tools_deny_withholds_effects_and_keeps_memory_writes() { run_isolated("probe_effect_tools_deny_withholds_effects", None); }
    #[test]
    fn supervisor_tools_effect_tools_deny_persists_across_resume() { run_isolated("probe_effect_tools_deny_persists_across_resume", None); }
    #[test]
    fn supervisor_tools_cross_effect_policy_switch_is_a_typed_error() { run_isolated("probe_cross_effect_policy_switch_rejected", None); }

    /// Writers a memory-write-denied session must never keep, pinned literally (whether or not this
    /// fixture registers each): removing one from the production exclusion list fails the probe.
    const PINNED_DENY_REMOVED: [&str; 20] = [
        "memory", "memory_apply_patch", "bash", "bash_input", "bash_output", "bash_resize", "kill_bash", "monitor", "powershell",
        "eval", "apply_patch", "lsp_rename", "lsp_format", "ast_grep_replace", "generate_image", "computer", "task", "dag", "workflow", "workpool",
    ];
    /// Read and conversation tools a denied session keeps whenever the baseline registers them.
    const PINNED_DENY_KEPT: [&str; 6] = ["read", "edit", "grep", "find", "ls", "todo"];

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_memory_write_deny_allowed_set_is_exact"]
    async fn probe_memory_write_deny_allowed_set() {
        let root = probe_root();
        let baseline = mount_with_context(&root, &[], None).await.expect("baseline mounts");
        let before = registered_tools(&baseline);
        baseline.runtime.dispose().await;
        drop(baseline);
        // Non-vacuous: the pinned fixture registers every tool the denial must remove.
        for name in MEMORY_WRITE_TOOLS.into_iter().chain(["bash", "bash_input", "monitor", "read", "write", "edit"]) {
            assert!(before.contains(name), "baseline registers {name}: {before:?}");
        }
        for name in SUPERVISOR_TOOLS { assert!(!before.contains(name), "{name} needs supervisor_tools=work"); }

        let restricted = mount_with_context(&root, &[("memory_write", "deny"), ("tools_exclude", "write"), ("supervisor_tools", "work")], None).await
            .expect("restricted session mounts");
        let after = registered_tools(&restricted);
        let mut expected: std::collections::BTreeSet<String> = before.iter()
            .filter(|name| !maho_omo_memory::write_policy::memory_write_deny_excludes_tool(name) && name.as_str() != "write").cloned().collect();
        expected.extend(SUPERVISOR_TOOLS.map(str::to_owned));
        assert_eq!(after, expected, "the denial withholds exactly the memory writers, shells, code execution and spawners plus the named exclusions");
        for name in ["read", "edit", "grep"] { assert_eq!(after.contains(name), before.contains(name), "{name} is not a policy-bypassing writer"); }
        for name in PINNED_DENY_REMOVED { assert!(!after.contains(name), "{name} must be withheld: {after:?}"); }
        let families: Vec<&String> = after.iter().filter(|name| name.starts_with("task_") || name.starts_with("team_")).collect();
        assert!(families.is_empty(), "the task/team families must be withheld: {families:?}");
        for name in PINNED_DENY_KEPT { assert_eq!(after.contains(name), before.contains(name), "{name} stays: {after:?}"); }
        let removed: Vec<&String> = before.difference(&after).collect();
        eprintln!("memory_write=deny removed: {removed:?}\nmemory_write=deny allowed set: {after:?}");
        let active = restricted.session().get_active_tool_names();
        for name in ["bash", "write", "memory", "memory_apply_patch"] { assert!(!active.iter().any(|tool| tool == name), "{name} is not active"); }
        let withheld = restricted.session().withheld_excluded_tool_names();
        for name in ["bash", "write", "memory", "memory_apply_patch"] { assert!(withheld.iter().any(|tool| tool == name), "{name} was offered and withheld: {withheld:?}"); }
        restricted.runtime.dispose().await;
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_reject_unknown_and_malformed_policy_with_typed_errors"]
    async fn probe_reject_unknown_and_malformed_policy() {
        let root = probe_root();
        for context in [
            [("tools_exclude", "bash,no_such_tool")],
            [("tools_exclude", "bash;rm -rf /")],
            [("tools_exclude", "bash,,write")],
            [("memory_write", "allow")],
            [("memory_write", "Deny")],
            [("supervisor_tools", "admin")],
        ] {
            let error = mount_with_context(&root, &context, None).await.err().unwrap_or_else(|| panic!("{context:?} must be rejected"));
            assert!(error.starts_with("invalid_session_context: "), "{context:?}: {error}");
            assert_eq!(maho_rpc::rpc_input_validation::open_session_error_code(&format!("open_failed: {error}")), Some("invalid_session_context"), "{error}");
        }
        let error = mount_with_context(&root, &[("tools_exclude", "bash,no_such_tool")], None).await.err().expect("unknown tool");
        assert!(error.contains("no_such_tool") && !error.contains("bash"), "only the unknown name is reported: {error}");
        // An MCP tool registers when its server connects, after the open: its name cannot be checked yet.
        let deferred = mount_with_context(&root, &[("tools_exclude", "mcp_not_connected_yet")], None).await.expect("deferred MCP name");
        deferred.runtime.dispose().await;
    }

    fn persisted_manager(root: &std::path::Path, id: &str) -> (maho_core::session_manager::SessionManager, String, String) {
        let project = root.join("project").to_string_lossy().into_owned();
        let sessions = root.join("sessions").to_string_lossy().into_owned();
        let manager = maho_core::session_manager::SessionManager::create(&project, Some(&sessions), Some(maho_core::session_manager::NewSessionOptions {
            id: Some(id.to_owned()), ..Default::default()
        }));
        (manager, project, sessions)
    }

    /// Appends one complete turn, which flushes the session file; returns its path.
    fn persist_turn(mounted: &maho_cli::cli::host_runtime::MountedRuntime) -> String {
        mounted.session().with_session_manager_mut(|manager| {
            manager.append_message(serde_json::json!({"role":"user","content":"decide","timestamp":1}));
            manager.append_message(serde_json::json!({"role":"assistant","content":[{"type":"text","text":"decided"}],"api":"faux","provider":"faux","model":"faux-1",
                "usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,"cost":{"input":0.0,"output":0.0,"cacheRead":0.0,"cacheWrite":0.0,"total":0.0}},
                "stopReason":"stop","timestamp":2}));
            manager.session_file().map(str::to_owned)
        }).expect("persisted session")
    }

    fn latest_policy(mounted: &maho_cli::cli::host_runtime::MountedRuntime) -> Option<serde_json::Value> {
        mounted.session().with_session_manager(|manager| manager.entries()).into_iter().rev()
            .find(|entry| entry["type"] == "custom" && entry["customType"] == maho_cli::cli::session_policy::SESSION_POLICY_CUSTOM_TYPE)
            .map(|entry| entry["data"].clone())
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_policy_survives_session_replacement"]
    async fn probe_policy_survives_session_replacement() {
        let root = probe_root();
        let (manager, _, _) = persisted_manager(&root, "supervisor-replaced");
        let mut mounted = mount_with_context(&root, &[("memory_write", "deny"), ("tools_exclude", "write")], Some(manager)).await.expect("open mounts");
        let first = persist_turn(&mounted);
        let record = serde_json::json!({"memoryWrite":"deny","toolsExclude":["write"]});
        assert_eq!(latest_policy(&mounted), Some(record.clone()));
        assert!(mounted.runtime.new_session(Default::default()).await.expect("new session"), "new_session is not cancelled");
        let replaced = persist_turn(&mounted);
        assert_ne!(replaced, first, "the runtime now shows a new session file");
        assert_eq!(latest_policy(&mounted), Some(record), "the replacement session records the same role");
        let tools = registered_tools(&mounted);
        for name in ["write", "bash", "memory", "memory_apply_patch"] { assert!(!tools.contains(name), "{name} stays withheld after new_session: {tools:?}"); }
        assert!(tools.contains("read"), "{tools:?}");
        mounted.runtime.dispose().await;
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_fail_the_open_when_default_tools_filter_them"]
    async fn probe_work_tools_filtered_by_default_tools() {
        let root = probe_root();
        let error = mount_with_context(&root, &[("supervisor_tools", "work")], None).await.err().expect("a supervisor session without its work tools must not open");
        assert!(error.starts_with("invalid_session_context: ") && error.contains("supervisor_submit_work"), "{error}");
        // Positive control: the same settings open fine when the work tools are not requested.
        let plain = mount_with_context(&root, &[], None).await.expect("plain session mounts");
        plain.runtime.dispose().await;
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_policy_persists_across_resume"]
    async fn probe_policy_persists_across_resume() {
        let root = probe_root();
        let dir = root.as_path();
        let (manager, project, sessions) = persisted_manager(dir, "supervisor-decision");
        let first = mount_with_context(dir, &[("memory_write", "deny"), ("tools_exclude", "bash"), ("supervisor_tools", "work")], Some(manager)).await
            .expect("first open mounts");
        let path = persist_turn(&first);
        first.runtime.dispose().await;
        drop(first);
        let persisted = std::fs::read_to_string(&path).expect("session file");
        let policy = persisted.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|entry| entry["customType"] == maho_cli::cli::session_policy::SESSION_POLICY_CUSTOM_TYPE).expect("policy entry");
        assert_eq!(policy["data"], serde_json::json!({"memoryWrite":"deny","toolsExclude":["bash"]}));

        // Stale client state: the resume names no policy, yet the recorded restrictions still hold.
        let reopen = |extra: &'static [(&'static str, &'static str)]| {
            let manager = maho_core::session_manager::SessionManager::open(&path, Some(&sessions), Some(&project), None).expect("reopen");
            mount_with_context(dir, extra, Some(manager))
        };
        let resumed = reopen(&[]).await.expect("resume mounts");
        let tools = registered_tools(&resumed);
        for name in ["bash"].into_iter().chain(MEMORY_WRITE_TOOLS).chain(SUPERVISOR_TOOLS) { assert!(!tools.contains(name), "{name} stays unavailable after resume: {tools:?}"); }
        assert!(tools.contains("write") && tools.contains("read"), "unrestricted tools stay: {tools:?}");
        resumed.runtime.dispose().await;
        drop(resumed);

        // A later open may add restrictions and re-request the work tools, never lift the denial.
        let widened = reopen(&[("tools_exclude", "write"), ("supervisor_tools", "work")]).await.expect("restricting resume mounts");
        let tools = registered_tools(&widened);
        for name in ["bash", "write"].into_iter().chain(MEMORY_WRITE_TOOLS) { assert!(!tools.contains(name), "{name}: {tools:?}"); }
        for name in SUPERVISOR_TOOLS { assert!(tools.contains(name), "{name}: {tools:?}"); }
        assert_eq!(latest_policy(&widened), Some(serde_json::json!({"memoryWrite":"deny","toolsExclude":["bash","write"]})));
        widened.runtime.dispose().await;
    }

    /// The memory identity this open bound (its binding entry) and that identity's paths.
    fn bound_identity(mounted: &maho_cli::cli::host_runtime::MountedRuntime) -> (String, memory_core::identity::layout::MemoryIdentityPaths) {
        let identity = mounted.session().with_session_manager(|manager| manager.entries()).into_iter().rev()
            .find(|entry| entry["type"] == "custom" && entry["customType"] == maho_omo_memory::binding::MEMORY_BINDING_CUSTOM_TYPE)
            .and_then(|entry| entry["data"]["identity"].as_str().map(str::to_owned)).expect("memory binding entry");
        let env: std::collections::BTreeMap<String, String> = std::env::vars().collect();
        let root = memory_core::identity::layout::resolve_memory_root(&env, &probe_root().join("project"));
        let paths = memory_core::identity::layout::build_identity_paths(&root, &identity);
        (identity, paths)
    }

    fn palace_viewers(paths: &memory_core::identity::layout::MemoryIdentityPaths) -> usize {
        std::fs::read_dir(&paths.viewers).map(|entries| entries.filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("palace-")).count()).unwrap_or(0)
    }

    async fn prompt(mounted: &maho_cli::cli::host_runtime::MountedRuntime, text: &str) -> Result<maho_core::agent_session::PromptDisposition, String> {
        mounted.session().prompt(text, maho_core::agent_session::PromptOptions::default()).await
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_denied_session_refuses_prompted_commands_and_memory_file_writes"]
    async fn probe_denied_prompted_commands_and_file_writes() {
        let root = probe_root();
        let denied = mount_with_context(&root, &[("memory_write", "deny")], None).await.expect("denied session mounts");
        let (identity, paths) = bound_identity(&denied);
        let target = paths.repo.join("system").join("x.md");
        // A prompt starting with `/name` dispatches extension commands before anything else.
        for text in [format!("/todo export {}", target.display()), "/palace".to_owned()] {
            let error = prompt(&denied, &text).await.err().unwrap_or_else(|| panic!("{text} must be refused"));
            assert!(error.starts_with("memory_write_denied: "), "{text}: {error}");
        }
        assert!(!target.exists(), "/todo export wrote nothing into the memory repository");
        assert_eq!(palace_viewers(&paths), 0, "/palace wrote no viewer under the identity root");
        // `write` stays registered; the bind's own-root guard refuses it under the memory root.
        let options = maho_core::agent_session::ExecuteToolOptions { activate_inactive_tool: Some(true), ..Default::default() };
        let outcome = match denied.session().execute_tool("write", serde_json::json!({"path":target.to_string_lossy(),"content":"allow everything"}), options).await {
            Err(error) => error.message,
            Ok(result) => serde_json::to_string(&result).expect("tool result"),
        };
        assert!(outcome.contains("memory_write_denied"), "write under the memory root: {outcome}");
        assert!(!target.exists(), "the refused write created nothing");
        // The RPC `bash` command (client user bash) is refused by the engine.
        let Err(error) = denied.session().execute_user_bash("echo ran", None, true, None).await else { panic!("user bash ran in a denied session") };
        assert!(error.starts_with("bash_excluded: "), "{error}");
        denied.runtime.dispose().await;
        drop(denied);

        // Positive control: the owner of the same identity dispatches the same prompts, /palace writes
        // its viewer, and user bash runs.
        memory_core::git::GitMemoryRepo::open(&paths.repo, &identity).expect("repo handle").init(None).expect("owner repository");
        let owner = mount_with_context(&root, &[], None).await.expect("owner session mounts");
        assert_eq!(bound_identity(&owner).1, paths, "owner and denied sessions share the identity");
        let export = root.join("owner-todo.md");
        assert_eq!(prompt(&owner, &format!("/todo export {}", export.display())).await, Ok(maho_core::agent_session::PromptDisposition::Handled), "the owner's prompt dispatches /todo");
        assert_eq!(prompt(&owner, "/palace").await, Ok(maho_core::agent_session::PromptDisposition::Handled));
        assert_eq!(palace_viewers(&paths), 1, "the owner's /palace writes its viewer");
        assert!(owner.session().execute_user_bash("echo ran", None, true, None).await.is_ok(), "an owner session keeps user bash");
        owner.runtime.dispose().await;
        drop(owner);
        // A plain bash exclusion hides bash from the model only: the user's `!cmd` still runs.
        let excluded = mount_with_context(&root, &[("tools_exclude", "bash")], None).await.expect("bash-excluded session mounts");
        assert!(!registered_tools(&excluded).contains("bash"), "the model loses bash");
        assert!(excluded.session().execute_user_bash("echo ran", None, true, None).await.is_ok(), "tools_exclude=bash keeps user bash");
        excluded.runtime.dispose().await;
    }

    fn recorded_denial(path: &str) -> bool {
        std::fs::read_to_string(path).expect("session file").lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|entry| entry["type"] == "custom" && (entry["data"]["memoryWrite"] == "deny"))
    }

    async fn mount_persisted(root: &std::path::Path, id: &str, context: &[(&str, &str)]) -> Result<maho_cli::cli::host_runtime::MountedRuntime, String> {
        let (manager, _, _) = persisted_manager(root, id);
        mount_with_context(root, context, Some(manager)).await
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_cross_role_switch_is_a_typed_error"]
    async fn probe_cross_role_switch_rejected() {
        let root = probe_root();
        let owner = mount_persisted(&root, "role-owner", &[]).await.expect("owner mounts");
        let owner_file = persist_turn(&owner);
        owner.runtime.dispose().await;
        drop(owner);
        let other = mount_persisted(&root, "role-denied-other", &[("memory_write", "deny")]).await.expect("second denied session mounts");
        let other_file = persist_turn(&other);
        other.runtime.dispose().await;
        drop(other);

        let mut denied = mount_persisted(&root, "role-denied", &[("memory_write", "deny")]).await.expect("denied mounts");
        persist_turn(&denied);
        let error = denied.runtime.switch_session(&owner_file, Default::default()).await.expect_err("a denied runtime cannot switch into an owner session");
        assert!(error.to_string().starts_with("invalid_session_context: "), "{error}");
        assert!(!recorded_denial(&owner_file), "the owner session file gained no denial");
        assert!(denied.runtime.switch_session(&other_file, Default::default()).await.expect("a same-role switch proceeds"), "not cancelled");
        denied.runtime.dispose().await;
        drop(denied);

        let mut unrestricted = mount_persisted(&root, "role-owner-2", &[]).await.expect("unrestricted mounts");
        let error = unrestricted.runtime.switch_session(&other_file, Default::default()).await.expect_err("an unrestricted runtime cannot switch into a denied session");
        assert!(error.to_string().starts_with("invalid_session_context: "), "{error}");
        assert!(registered_tools(&unrestricted).contains("bash"), "the refused switch left the unrestricted session in place");
        unrestricted.runtime.dispose().await;
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_memory_write_deny_fails_the_open_without_a_memory_bind"]
    async fn probe_deny_without_memory_bind_fails_the_open() {
        let root = probe_root();
        let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("isolated HOME"));
        std::fs::write(home.join(".omo").join("omo.json"), "{\"memory\":{\"enabled\":false}}\n").expect("memory disabled");
        let error = mount_with_context(&root, &[("memory_write", "deny")], None).await.err().expect("a denied session without a memory bind must not open");
        assert!(error.starts_with("invalid_session_context: ") && error.contains("memory_write=deny"), "{error}");
        assert_eq!(maho_rpc::rpc_input_validation::open_session_error_code(&format!("open_failed: {error}")), Some("invalid_session_context"));
        // effect_tools=deny needs memory too: the memory component carries its MCP refusal.
        let error = mount_with_context(&root, &EFFECT_DENY_CONTEXT, None).await.err().expect("an effect-denied session without a memory bind must not open");
        assert!(error.starts_with("invalid_session_context: ") && error.contains("effect_tools=deny"), "{error}");
        // Positive control: without the denial the same configuration opens.
        let plain = mount_with_context(&root, &[], None).await.expect("an unrestricted session opens without memory");
        plain.runtime.dispose().await;
    }

    /// The `effect_tools = "deny"` context Ferryx sends for its owner-ingest session.
    const EFFECT_DENY_CONTEXT: [(&str, &str); 2] = [("effect_tools", "deny"), ("tools_exclude", "bash,edit,write")];

    /// A tool `effect_tools = "deny"` withholds: the memory-write-deny effect set without the memory writers.
    fn effect_deny_excludes(name: &str) -> bool {
        maho_omo_memory::write_policy::memory_write_deny_excludes_tool(name) && !MEMORY_WRITE_TOOLS.contains(&name)
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_effect_tools_deny_withholds_effects_and_keeps_memory_writes"]
    async fn probe_effect_tools_deny_withholds_effects() {
        let root = probe_root();
        for value in ["allow", "Deny", ""] {
            let error = mount_with_context(&root, &[("effect_tools", value)], None).await.err().unwrap_or_else(|| panic!("effect_tools={value:?} must be rejected"));
            assert!(error.starts_with("invalid_session_context: ") && error.contains("effect_tools"), "{value:?}: {error}");
            assert_eq!(maho_rpc::rpc_input_validation::open_session_error_code(&format!("open_failed: {error}")), Some("invalid_session_context"), "{error}");
        }
        let baseline = mount_with_context(&root, &[], None).await.expect("baseline mounts");
        let before = registered_tools(&baseline);
        baseline.runtime.dispose().await;
        drop(baseline);
        // Non-vacuous: the pinned fixture registers the writers the policy must remove and the memory writers it must keep.
        for name in MEMORY_WRITE_TOOLS.into_iter().chain(["bash", "bash_input", "monitor", "read", "write", "edit"]) {
            assert!(before.contains(name), "baseline registers {name}: {before:?}");
        }

        let effect = mount_with_context(&root, &EFFECT_DENY_CONTEXT, None).await.expect("effect-denied session mounts");
        let after = registered_tools(&effect);
        let expected: std::collections::BTreeSet<String> = before.iter()
            .filter(|name| !effect_deny_excludes(name) && !["bash", "edit", "write"].contains(&name.as_str())).cloned().collect();
        assert_eq!(after, expected, "effect_tools=deny withholds exactly the effect set plus the named exclusions");
        for name in PINNED_DENY_REMOVED.into_iter().filter(|name| !MEMORY_WRITE_TOOLS.contains(name)) {
            assert!(!after.contains(name), "{name} must be withheld: {after:?}");
        }
        let families: Vec<&String> = after.iter().filter(|name| name.starts_with("task_") || name.starts_with("team_")).collect();
        assert!(families.is_empty(), "the task/team families must be withheld: {families:?}");
        for name in MEMORY_WRITE_TOOLS.into_iter().chain(["read"]) { assert!(after.contains(name), "{name} stays: {after:?}"); }
        assert_eq!(latest_policy(&effect), Some(serde_json::json!({"memoryWrite":"allow","toolsExclude":["bash","edit","write"],"effectTools":"deny"})));
        let entries = effect.session().with_session_manager(|manager| manager.entries());
        assert!(!entries.iter().any(|entry| entry["data"]["memoryWrite"] == "deny"), "effect_tools=deny records no memory-write denial");
        let Err(error) = effect.session().execute_user_bash("echo ran", None, true, None).await else { panic!("user bash ran in an effect-denied session") };
        assert!(error.starts_with("bash_excluded: "), "{error}");
        // Prompt-dispatched commands outside the allowlist and the memory commands are refused.
        let export = root.join("effect-todo.md");
        let Err(error) = prompt(&effect, &format!("/todo export {}", export.display())).await else {
            panic!("/todo export must be refused");
        };
        assert!(error.starts_with("effect_tools_denied: "), "{error}");
        assert!(!export.exists(), "/todo export wrote nothing");

        // Owner positive control: the same session still writes memory.
        let (identity, paths) = bound_identity(&effect);
        memory_core::git::GitMemoryRepo::open(&paths.repo, &identity).expect("repo handle").init(None).expect("owner repository");
        let options = maho_core::agent_session::ExecuteToolOptions { activate_inactive_tool: Some(true), ..Default::default() };
        let written = effect.session().execute_tool("memory", serde_json::json!({"command":"create","reason":"owner taught a rule","file_path":"system/effect-rule.md",
            "description":"Effect rule","file_text":"Keep PRs small."}), options).await
            .unwrap_or_else(|error| panic!("memory write in an effect-denied session: {}", error.message));
        assert_ne!(written.is_error, Some(true), "the memory write succeeds: {written:?}");
        assert!(!serde_json::to_string(&written).expect("tool result").contains("memory_write_denied"), "{written:?}");
        effect.runtime.dispose().await;
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_effect_tools_deny_persists_across_resume"]
    async fn probe_effect_tools_deny_persists_across_resume() {
        let root = probe_root();
        let dir = root.as_path();
        let (manager, project, sessions) = persisted_manager(dir, "supervisor-ingest");
        let first = mount_with_context(dir, &EFFECT_DENY_CONTEXT, Some(manager)).await.expect("first open mounts");
        let path = persist_turn(&first);
        first.runtime.dispose().await;
        drop(first);
        let record = serde_json::json!({"memoryWrite":"allow","toolsExclude":["bash","edit","write"],"effectTools":"deny"});
        let persisted = std::fs::read_to_string(&path).expect("session file");
        let policy = persisted.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|entry| entry["customType"] == maho_cli::cli::session_policy::SESSION_POLICY_CUSTOM_TYPE).expect("policy entry");
        assert_eq!(policy["data"], record);

        // Restart: the resume names no policy, yet the recorded effect denial still holds.
        let manager = maho_core::session_manager::SessionManager::open(&path, Some(&sessions), Some(&project), None).expect("reopen");
        let mut resumed = mount_with_context(dir, &[], Some(manager)).await.expect("resume mounts");
        let tools = registered_tools(&resumed);
        for name in ["bash", "bash_input", "monitor", "edit", "write"] { assert!(!tools.contains(name), "{name} stays withheld after resume: {tools:?}"); }
        assert!(!tools.iter().any(|name| effect_deny_excludes(name)), "the effect set stays withheld: {tools:?}");
        for name in MEMORY_WRITE_TOOLS.into_iter().chain(["read"]) { assert!(tools.contains(name), "{name} stays after resume: {tools:?}"); }
        let Err(error) = resumed.session().execute_user_bash("echo ran", None, true, None).await else { panic!("user bash ran after resume") };
        assert!(error.starts_with("bash_excluded: "), "{error}");

        // Replacement keeps the policy too.
        assert!(resumed.runtime.new_session(Default::default()).await.expect("new session"), "new_session is not cancelled");
        persist_turn(&resumed);
        assert_eq!(latest_policy(&resumed), Some(record), "the replacement session records the same policy");
        let tools = registered_tools(&resumed);
        assert!(!tools.contains("bash") && tools.contains("memory"), "{tools:?}");
        resumed.runtime.dispose().await;
    }

    fn recorded_effect_denial(path: &str) -> bool {
        std::fs::read_to_string(path).expect("session file").lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|entry| entry["type"] == "custom" && entry["data"]["effectTools"] == "deny")
    }

    #[tokio::test]
    #[ignore = "runs in an isolated HOME through supervisor_tools_cross_effect_policy_switch_is_a_typed_error"]
    async fn probe_cross_effect_policy_switch_rejected() {
        let root = probe_root();
        let owner = mount_persisted(&root, "effect-owner", &[]).await.expect("owner mounts");
        let owner_file = persist_turn(&owner);
        owner.runtime.dispose().await;
        drop(owner);
        let other = mount_persisted(&root, "effect-other", &EFFECT_DENY_CONTEXT).await.expect("second effect-denied session mounts");
        let other_file = persist_turn(&other);
        other.runtime.dispose().await;
        drop(other);

        let mut effect = mount_persisted(&root, "effect-current", &EFFECT_DENY_CONTEXT).await.expect("effect-denied mounts");
        persist_turn(&effect);
        let error = effect.runtime.switch_session(&owner_file, Default::default()).await.expect_err("an effect-denied runtime cannot switch into an unrestricted session");
        assert!(error.to_string().starts_with("invalid_session_context: "), "{error}");
        assert!(!recorded_effect_denial(&owner_file), "the owner session file gained no effect denial");
        assert!(effect.runtime.switch_session(&other_file, Default::default()).await.expect("a same-policy switch proceeds"), "not cancelled");
        effect.runtime.dispose().await;
        drop(effect);

        let mut unrestricted = mount_persisted(&root, "effect-owner-2", &[]).await.expect("unrestricted mounts");
        let error = unrestricted.runtime.switch_session(&other_file, Default::default()).await.expect_err("an unrestricted runtime cannot switch into an effect-denied session");
        assert!(error.to_string().starts_with("invalid_session_context: "), "{error}");
        assert!(registered_tools(&unrestricted).contains("bash"), "the refused switch left the unrestricted session in place");
        unrestricted.runtime.dispose().await;
        drop(unrestricted);

        let mut denied = mount_persisted(&root, "effect-memory-denied", &[("memory_write", "deny")]).await.expect("memory-write-denied mounts");
        let error = denied.runtime.switch_session(&other_file, Default::default()).await.expect_err("a memory-write-denied runtime cannot switch into an effect-denied session");
        assert!(error.to_string().starts_with("invalid_session_context: "), "{error}");
        denied.runtime.dispose().await;
    }
}
