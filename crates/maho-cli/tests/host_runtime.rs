use maho_cli::cli::args::Args;
use maho_cli::cli::host_runtime::{mount_agent_session_runtime, resolve_cli_initial_model, resolve_cli_path, CliRuntimeConfiguration, CliRuntimeRequest};
use maho_core::project_trust::AppMode;

#[tokio::test]
async fn fresh_session_reports_the_origin_its_model_came_from() {
    let dir = tempfile::tempdir().expect("isolated model runtime");
    let agent = dir.path().join("agent");
    std::fs::create_dir_all(&agent).expect("agent dir");
    let cwd = dir.path().to_string_lossy();
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        providers: Some(Vec::new()), ..Default::default()
    });
    let settings = maho_core::settings_manager::SettingsManager::create(&cwd, &agent.to_string_lossy(), &cwd, false);
    let pinned = resolve_cli_initial_model(
        Some(("faux".to_owned(), "faux-1".to_owned())), None, false, &settings, &runtime, &[]).await;
    assert_eq!(pinned.provenance, Some("cli"));
    let automatic = resolve_cli_initial_model(None, None, false, &settings, &runtime, &[]).await;
    assert_eq!(automatic.provenance, Some("first-available"));
    let continuing = resolve_cli_initial_model(None, None, true, &settings, &runtime, &[]).await;
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
    });
    std::fs::write(agent.join("settings.json"), "{\"defaultProvider\":\"faux\",\"defaultModel\":\"qa-beta\"}\n")
        .expect("settings fixture");
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(dir.path().join("models.json")), auth_path: Some(dir.path().join("auth.json")),
        providers: Some(Vec::new()), ..Default::default()
    });
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
    let resolved = resolve_cli_initial_model(None, None, false, &settings, &runtime, &scope.scoped_models).await;
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
        Some(("faux".to_owned(), "qa-beta".to_owned())), None, false, &settings, &runtime, &scope.scoped_models).await;
    assert_eq!(explicit.provenance, Some("cli"));
    assert_eq!(explicit.model.as_ref().map(|model| model.id.as_str()), Some("qa-beta"));
    assert_ne!(explicit.model.as_ref().map(|model| model.id.as_str()), scope.scoped_models.first().map(|scoped| scoped.model.id.as_str()));
}

#[tokio::test]
async fn a_resumed_session_resolves_no_model_and_keeps_no_origin() {
    let (_dir, provider, runtime, settings) = scoped_fixture();
    let scope = maho_core::model_resolver::resolve_model_scope_from_models(&["qa-alpha:high".to_owned()], &provider.models);
    let resumed = resolve_cli_initial_model(None, None, true, &settings, &runtime, &scope.scoped_models).await;
    assert_eq!(resumed.provenance, None);
    assert!(resumed.model.is_none());
    assert!(resumed.thinking_level.is_none());
    assert!(resumed.thinking_selection.is_none());
}

fn config(parsed: &Args) -> CliRuntimeConfiguration {
    CliRuntimeConfiguration::from_parsed(parsed, "/tmp/project", "/tmp/agent", AppMode::Interactive)
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
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(agent.join("models.json")), auth_path: Some(agent.join("auth.json")),
        providers: Some(Vec::new()), ..Default::default()
    });
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
    assert_eq!(profile.cwd, "/tmp/project");
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
    assert_eq!(resolve_cli_path("/w", "/abs/x"), "/abs/x");
    assert_eq!(resolve_cli_path("/w", "rel/x"), "/w/rel/x");
    let parsed = Args {
        skills: vec!["skills".to_owned()],
        prompt_templates: vec!["/abs/templates".to_owned()],
        themes: vec!["themes".to_owned()],
        extensions: vec!["ext".to_owned()],
        ..Default::default()
    };
    let config = config(&parsed);
    assert_eq!(config.resolved_skill_paths(), vec!["/tmp/project/skills".to_owned()]);
    assert_eq!(config.resolved_prompt_template_paths(), vec!["/abs/templates".to_owned()]);
    assert_eq!(config.resolved_theme_paths(), vec!["/tmp/project/themes".to_owned()]);
    assert_eq!(config.resolved_extension_paths(), vec!["/tmp/project/ext".to_owned()]);
}

#[test]
fn configuration_carries_the_parsed_session_directory() {
    let parsed = Args { session_dir: Some("/sessions".to_owned()), ..Default::default() };
    assert_eq!(config(&parsed).session_dir.as_deref(), Some("/sessions"));
}
