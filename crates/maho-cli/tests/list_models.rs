//! senpi cli/list-models.ts `listModels` / `formatTokenCount`.
use maho_cli::cli::list_models::{format_token_count, list_models};

fn runtime_with(
    models: &[(&str, bool)],
) -> (tempfile::TempDir, maho_core::model_runtime::ModelRuntime) {
    let dir = tempfile::tempdir().expect("isolated model runtime");
    let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(dir.path().join("models.json")),
            auth_path: Some(dir.path().join("auth.json")),
            providers: Some(Vec::new()),
            ..Default::default()
        },
    )
    .expect("valid model runtime");
    if !models.is_empty() {
        let provider = maho_ai::providers::faux::faux_provider(
            maho_ai::providers::faux::RegisterFauxProviderOptions {
                models: Some(
                    models
                        .iter()
                        .map(
                            |(id, reasoning)| maho_ai::providers::faux::FauxModelDefinition {
                                id: (*id).to_owned(),
                                reasoning: Some(*reasoning),
                                ..Default::default()
                            },
                        )
                        .collect(),
                ),
                ..Default::default()
            },
        )
        .expect("valid faux provider");
        runtime.register_native_provider(provider.provider.clone());
        runtime
            .register_provider(
                "faux",
                maho_core::provider_composer::ProviderConfigInput {
                    config: maho_core::model_config_schema::ModelsJsonProvider {
                        api_key: Some("fixture-key".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .expect("configured provider");
    }
    (dir, runtime)
}

#[test]
fn token_counts_render_whole_and_fractional_units() {
    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(200_000), "200K");
    assert_eq!(format_token_count(1_500), "1.5K");
    assert_eq!(format_token_count(1_000_000), "1M");
    assert_eq!(format_token_count(1_250_000), "1.3M");
    assert_eq!(format_token_count(1_150_000), "1.1M");
    assert_eq!(format_token_count(2_750), "2.8K");
}

#[test]
fn an_empty_catalog_answers_the_guidance_even_with_a_search() {
    let (_dir, runtime) = runtime_with(&[]);
    assert!(
        runtime.get_models(None).is_empty(),
        "no providers registered"
    );
    let guidance = maho_core::auth_guidance::format_no_models_available_message();
    assert_eq!(list_models(&runtime, None), guidance);
    assert_eq!(list_models(&runtime, Some("gpt")), guidance);
}

#[test]
fn a_search_without_matches_names_the_pattern_and_matches_sort_by_provider_then_id() {
    let (_dir, runtime) = runtime_with(&[("qa-zeta", true), ("qa-alpha", false)]);
    assert_eq!(
        list_models(&runtime, Some("zzzz-no-such-model-qqqq")),
        "No models matching \"zzzz-no-such-model-qqqq\""
    );
    let table = list_models(&runtime, Some("faux qa"));
    let lines: Vec<&str> = table.lines().collect();
    assert!(lines[0].starts_with("provider"), "{table}");
    let alpha = lines
        .iter()
        .position(|line| line.contains("qa-alpha"))
        .expect("qa-alpha row");
    let zeta = lines
        .iter()
        .position(|line| line.contains("qa-zeta"))
        .expect("qa-zeta row");
    assert!(alpha < zeta, "{table}");
    assert!(
        lines[zeta].split_whitespace().any(|cell| cell == "yes"),
        "reasoning column: {table}"
    );
}
