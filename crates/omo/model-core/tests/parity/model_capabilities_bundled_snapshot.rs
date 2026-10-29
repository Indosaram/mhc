use model_core::CapabilitySource;
use model_core::GetModelCapabilitiesInput;
use model_core::ResolutionMode;
use model_core::SnapshotSource;
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
