//! Single-source persona asset filenames (pin `personas/manifest.ts`).

/// Reflection persona filename.
pub const REFLECTION_PERSONA_FILENAME: &str = "reflection-persona.md";
/// Dream persona filename.
pub const DREAM_PERSONA_FILENAME: &str = "dream-persona.md";
/// Facts persona filename.
pub const FACTS_PERSONA_FILENAME: &str = "facts-persona.md";
/// Kibitzer persona filename.
pub const KIBITZER_PERSONA_FILENAME: &str = "kibitzer-persona.md";

/// Persona asset kinds carried by the manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PersonaAssetKind {
    Reflection,
    Dream,
    Facts,
    Kibitzer,
}

/// Filename of a persona asset kind.
pub fn persona_asset_filename(kind: PersonaAssetKind) -> &'static str {
    match kind {
        PersonaAssetKind::Reflection => REFLECTION_PERSONA_FILENAME,
        PersonaAssetKind::Dream => DREAM_PERSONA_FILENAME,
        PersonaAssetKind::Facts => FACTS_PERSONA_FILENAME,
        PersonaAssetKind::Kibitzer => KIBITZER_PERSONA_FILENAME,
    }
}

/// Every persona asset filename, in manifest order.
pub const PERSONA_ASSET_FILES: [&str; 4] = [
    REFLECTION_PERSONA_FILENAME,
    DREAM_PERSONA_FILENAME,
    FACTS_PERSONA_FILENAME,
    KIBITZER_PERSONA_FILENAME,
];

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
