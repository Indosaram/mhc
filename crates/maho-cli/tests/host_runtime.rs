use maho_cli::cli::args::Args;
use maho_cli::cli::host_runtime::{resolve_cli_path, CliRuntimeConfiguration};
use maho_core::project_trust::AppMode;

fn config(parsed: &Args) -> CliRuntimeConfiguration {
    CliRuntimeConfiguration::from_parsed(parsed, "/tmp/project", "/tmp/agent", AppMode::Interactive)
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
