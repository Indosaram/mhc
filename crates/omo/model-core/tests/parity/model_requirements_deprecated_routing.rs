use model_core::AGENT_MODEL_REQUIREMENTS;
use model_core::CATEGORY_MODEL_REQUIREMENTS;
use model_core::FallbackEntry;
use pretty_assertions::assert_eq;

#[test]
fn no_deprecated_haiku_or_gpt_nano_fallback_entry_routes_through_opencode() {
    let deprecated_models = ["claude-haiku-4-5", "gpt-5.4-nano"];

    let deprecated_opencode_entries: Vec<&FallbackEntry> = AGENT_MODEL_REQUIREMENTS
        .values()
        .chain(CATEGORY_MODEL_REQUIREMENTS.values())
        .flat_map(|requirement| &requirement.fallback_chain)
        .filter(|entry| {
            deprecated_models.contains(&entry.model.as_str())
                && entry.providers.iter().any(|p| p == "opencode")
        })
        .collect();

    assert_eq!(deprecated_opencode_entries, Vec::<&FallbackEntry>::new());
}
