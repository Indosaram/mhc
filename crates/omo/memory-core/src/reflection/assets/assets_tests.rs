use pretty_assertions::assert_eq;

use super::{
    DREAM_PERSONA_MARKDOWN, REFLECTION_PERSONA_MARKDOWN, load_dream_persona,
    load_reflection_persona,
};

#[test]
fn given_dream_persona_asset_when_loaded_then_it_equals_verbatim_markdown() {
    let persona = load_dream_persona();
    assert_eq!(persona.markdown, DREAM_PERSONA_MARKDOWN);
    assert!(!persona.sections.is_empty());

    let headings: Vec<&str> = persona
        .sections
        .iter()
        .map(|s| s.heading.as_str())
        .collect();
    assert!(headings.contains(&"Tools and Paths"));
    assert!(headings.contains(&"Phase 1: Investigate"));
    assert!(headings.contains(&"Phase 2: Consolidate"));
    assert!(headings.contains(&"Phase 3: Skill Audit"));
    assert!(headings.contains(&"Phase 4: People"));
    assert!(headings.contains(&"Phase 5: Review"));
    assert!(headings.contains(&"Phase 6: Commit"));
}

#[test]
fn given_reflection_persona_asset_when_loaded_then_it_equals_verbatim_markdown() {
    let persona = load_reflection_persona();
    assert_eq!(persona.markdown, REFLECTION_PERSONA_MARKDOWN);
    assert!(!persona.sections.is_empty());

    let headings: Vec<&str> = persona
        .sections
        .iter()
        .map(|s| s.heading.as_str())
        .collect();
    assert!(headings.contains(&"Tools and Paths"));
    assert!(headings.contains(&"Phase 1: Investigate"));
    assert!(headings.contains(&"Phase 2: Extract"));
    assert!(headings.contains(&"Phase 3: Update"));
    assert!(headings.contains(&"Phase 4: Review"));
    assert!(headings.contains(&"Phase 5: Commit"));
}

#[test]
fn given_reflection_persona_asset_when_trailer_keys_are_checked_then_runtime_keys_are_present() {
    let persona = load_reflection_persona();
    assert!(persona.markdown.contains("Generated-By:"));
    assert!(persona.markdown.contains("Agent-ID:"));
}
