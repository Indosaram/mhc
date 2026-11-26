//! Capability and family cases from `gpt-6.2.test.ts`; routing tables are tested separately.
use crate::support::strings;
use model_core::{
    GetModelCapabilitiesInput, ModelCapabilitiesSnapshot, SnapshotLimit, SnapshotModalities,
    get_bundled_model_capabilities_snapshot, get_model_capabilities, is_gpt6_astra_model,
};

#[test]
fn four_gpt_62_rows_supply_limits_even_without_a_generated_catalog() {
    let snapshot = get_bundled_model_capabilities_snapshot(&ModelCapabilitiesSnapshot::default());
    for id in [
        "gpt-6.2-astra",
        "gpt-6.2-astra-fast",
        "gpt-6.2-sol",
        "gpt-6.2-sol-fast",
    ] {
        let row = &snapshot.models[id];
        assert_eq!(
            row.limit,
            Some(SnapshotLimit {
                context: Some(1_050_000),
                input: Some(922_000),
                output: Some(128_000)
            }),
            "{id}"
        );
        let caps = get_model_capabilities(GetModelCapabilitiesInput {
            provider_id: "openai",
            model_id: id,
            bundled_snapshot: Some(&snapshot),
            ..Default::default()
        });
        assert_eq!(
            caps.reasoning_efforts,
            Some(strings(&["low", "medium", "high", "xhigh", "max"])),
            "{id}"
        );
        assert_eq!(caps.max_output_tokens, Some(128_000), "{id}");
        assert_eq!(caps.supports_temperature, Some(false), "{id}");
        assert_eq!(
            caps.modalities,
            Some(SnapshotModalities {
                input: Some(strings(&["text", "image"])),
                output: Some(strings(&["text"]))
            }),
            "{id}"
        );
    }
}

#[test]
fn astra_only_routing_distinguishes_sol_and_unrecognized_tiers() {
    for (selector, expected) in [
        ("chatgpt-subscription/gpt-6.2-astra", true),
        ("openai/gpt-6.2-astra-fast", true),
        ("chatgpt-subscription/gpt-6.2-astra-ultrafast:xhigh", true),
        ("openai/gpt-6-astra-ultrafast", true),
        ("chatgpt-subscription/gpt-6.2-sol", false),
        ("chatgpt-subscription/gpt-6.2-sol-ultrafast", false),
        ("openai/gpt-6.2-astra-fastest", false),
    ] {
        assert_eq!(is_gpt6_astra_model(Some(selector)), expected, "{selector}");
    }
}
