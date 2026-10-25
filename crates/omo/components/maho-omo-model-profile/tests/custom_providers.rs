use maho_omo_model_profile::builtin_profiles::builtin_model_profiles;
use maho_omo_model_profile::custom_providers::{DeclaredModelProvider, custom_model_providers};
use maho_omo_model_profile::resolve::{ModelProfileResolution, ResolveModelProfileInput, resolve_model_profile};

fn agent_dir_with(models_json: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("models.json"), models_json).expect("models.json");
    dir
}

/// The Claude rung id `daily-normal` heads with, read from the builtin table instead of restating a
/// model id: the case under test is the routing, not the id.
fn opus_rung_id() -> String {
    builtin_model_profiles()["daily-normal"].models[0].model.to_owned()
}

fn resolve_with(available: &[String], declared: &[DeclaredModelProvider]) -> ModelProfileResolution {
    resolve_model_profile(&ResolveModelProfileInput {
        profiles: None,
        active: "daily-normal",
        available_models: available,
        custom_providers: Some(declared),
    })
}

#[test]
fn lists_providers_with_their_own_models_in_declaration_order() {
    let dir = agent_dir_with(
        r#"{
      // JSONC, as senpi parses it
      "providers": {
        "proxy-b": { "baseUrl": "https://b.example/v1", "models": [{ "id": "declared-b" }] },
        "anthropic": { "baseUrl": "https://override.example" },
        "proxy-a": { "models": [{ "id": "declared-a" }, { "id": "glm-5.3" }] },
        "empty": { "models": [] },
        "malformed": { "models": [{}, { "id": 7 }, { "id": "" }] }
      },
    }"#,
    );
    assert_eq!(
        custom_model_providers(Some(dir.path())),
        [
            DeclaredModelProvider {
                provider: "proxy-b".to_owned(),
                model_ids: vec!["declared-b".to_owned()],
            },
            DeclaredModelProvider {
                provider: "proxy-a".to_owned(),
                model_ids: vec!["declared-a".to_owned(), "glm-5.3".to_owned()],
            },
        ]
    );
}

#[test]
fn no_agent_dir_no_file_or_malformed_providers_yield_no_providers() {
    assert!(custom_model_providers(None).is_empty());
    let without_file = tempfile::tempdir().expect("temp dir");
    assert!(custom_model_providers(Some(without_file.path())).is_empty());
    let malformed = agent_dir_with(r#"{ "providers": [] }"#);
    assert!(custom_model_providers(Some(malformed.path())).is_empty());
}

// Security contract (#9146): models.json entries are merged into a built-in provider's catalog, so a
// gateway the user extends with one model must not serve a builtin rung id the user never declared.
#[test]
fn a_gateway_extended_with_an_unrelated_model_does_not_serve_the_lanes_rung_id() {
    let dir = agent_dir_with(r#"{ "providers": { "openrouter": { "models": [{ "id": "my-finetune" }] } } }"#);
    let available = vec![
        format!("openrouter/{}", opus_rung_id()),
        "openrouter/my-finetune".to_owned(),
    ];
    let result = resolve_with(&available, &custom_model_providers(Some(dir.path())));
    assert!(matches!(result, ModelProfileResolution::Unavailable { .. }));
}

#[test]
fn the_same_gateway_declaring_the_rung_id_serves_the_lane() {
    let dir = agent_dir_with(&format!(
        r#"{{ "providers": {{ "openrouter": {{ "models": [{{ "id": "{}" }}] }} }} }}"#,
        opus_rung_id()
    ));
    let available = vec![format!("openrouter/{}", opus_rung_id())];
    let result = resolve_with(&available, &custom_model_providers(Some(dir.path())));
    let (provider, model_id) = match result {
        ModelProfileResolution::Resolved { provider, model_id, .. } => (provider, model_id),
        other => panic!("expected the declared provider to serve the lane, got {other:?}"),
    };
    assert_eq!(provider, "openrouter");
    assert_eq!(model_id, opus_rung_id());
}
