use indexmap::IndexMap;
use model_core::FamilySource;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelCapabilitiesSnapshot;
use model_core::ResolutionMode;
use model_core::SnapshotSource;
use model_core::get_model_capabilities;
use pretty_assertions::assert_eq;

#[test]
fn detects_opencode_go_qwen_max_models_through_the_heuristic_fallback() {
    let bundled_snapshot = ModelCapabilitiesSnapshot {
        generated_at: "2026-03-25T00:00:00.000Z".to_string(),
        source_url: "https://models.dev/api.json".to_string(),
        models: IndexMap::new(),
    };
    let model_id = "qwen3.7-max";

    let result = get_model_capabilities(GetModelCapabilitiesInput {
        provider_id: "opencode-go",
        model_id,
        bundled_snapshot: Some(&bundled_snapshot),
        ..Default::default()
    });

    assert_eq!(result.canonical_model_id, model_id);
    assert_eq!(result.family.as_deref(), Some("qwen"));
    assert_eq!(
        result.diagnostics.resolution_mode,
        ResolutionMode::HeuristicBacked
    );
    assert_eq!(result.diagnostics.snapshot, SnapshotSource::None);
    assert_eq!(result.diagnostics.family, FamilySource::Heuristic);
}
