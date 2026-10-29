use super::*;

#[test]
fn manifest_lists_every_persona_filename_in_order() {
    assert_eq!(
        PERSONA_ASSET_FILES,
        [
            "reflection-persona.md",
            "dream-persona.md",
            "facts-persona.md",
            "kibitzer-persona.md",
        ]
    );
}

#[test]
fn filename_lookup_covers_every_kind() {
    assert_eq!(
        persona_asset_filename(PersonaAssetKind::Reflection),
        REFLECTION_PERSONA_FILENAME
    );
    assert_eq!(
        persona_asset_filename(PersonaAssetKind::Dream),
        DREAM_PERSONA_FILENAME
    );
    assert_eq!(
        persona_asset_filename(PersonaAssetKind::Facts),
        FACTS_PERSONA_FILENAME
    );
    assert_eq!(
        persona_asset_filename(PersonaAssetKind::Kibitzer),
        KIBITZER_PERSONA_FILENAME
    );
}

#[test]
fn shipped_reflection_and_dream_assets_match_their_manifest_names() {
    assert!(!crate::reflection::assets::REFLECTION_PERSONA_MARKDOWN.is_empty());
    assert!(!crate::reflection::assets::DREAM_PERSONA_MARKDOWN.is_empty());
    assert_eq!(REFLECTION_PERSONA_FILENAME, "reflection-persona.md");
    assert_eq!(DREAM_PERSONA_FILENAME, "dream-persona.md");
}
