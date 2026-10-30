use model_core::CapabilitySource;
use model_core::FABLE_55_LAUNCH_ID;
use model_core::GetModelCapabilitiesInput;
use model_core::ModelCapabilitiesSnapshot;
use model_core::ModelCapabilitiesSnapshotEntry;
use model_core::ResolutionMode;
use model_core::SnapshotLimit;
use model_core::SnapshotModalities;
use model_core::SnapshotSource;
use model_core::get_bundled_model_capabilities_snapshot;
use model_core::get_model_capabilities;
use pretty_assertions::assert_eq;

use crate::support::bundled_snapshot;

#[test]
fn keeps_gpt_4_1_openai_variants_marked_as_supporting_tool_calls() {
    let bundled_snapshot = bundled_snapshot();

    for model_id in [
        "openai/gpt-4.1",
        "openai/gpt-4.1-mini",
        "openai/gpt-4.1-nano",
    ] {
        let result = get_model_capabilities(GetModelCapabilitiesInput {
            provider_id: "openai",
            model_id,
            bundled_snapshot: Some(&bundled_snapshot),
            ..Default::default()
        });

        assert_eq!(result.tool_call, Some(true), "{model_id}");
        assert_eq!(
            result.diagnostics.resolution_mode,
            ResolutionMode::SnapshotBacked,
            "{model_id}"
        );
        assert_eq!(
            result.diagnostics.snapshot,
            SnapshotSource::BundledSnapshot,
            "{model_id}"
        );
        assert_eq!(
            result.diagnostics.tool_call,
            CapabilitySource::BundledSnapshot,
            "{model_id}"
        );
    }
}

// The launch placeholder must never hide real data once the generated catalog lists the model.
#[test]
fn generated_catalog_data_wins_over_the_launch_placeholder() {
    let generated = ModelCapabilitiesSnapshotEntry {
        id: FABLE_55_LAUNCH_ID.to_string(),
        family: Some("claude-fable".to_string()),
        reasoning: Some(true),
        temperature: Some(false),
        tool_call: Some(true),
        modalities: Some(SnapshotModalities {
            input: Some(vec!["text".to_string(), "image".to_string()]),
            output: Some(vec!["text".to_string()]),
        }),
        limit: Some(SnapshotLimit {
            context: Some(1_000_000),
            input: None,
            output: Some(128_000),
        }),
    };
    let snapshot = get_bundled_model_capabilities_snapshot(&ModelCapabilitiesSnapshot {
        generated_at: "2026-10-22T00:00:00Z".to_string(),
        source_url: "fixture".to_string(),
        models: [(FABLE_55_LAUNCH_ID.to_string(), generated.clone())]
            .into_iter()
            .collect(),
    });

    assert_eq!(snapshot.models.get(FABLE_55_LAUNCH_ID), Some(&generated));
}
