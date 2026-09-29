use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_load_facts_persona_non_empty_and_valid() {
    let persona = load_facts_persona();
    assert_eq!(persona.is_empty(), false);
    assert_eq!(persona.starts_with("# Facts extractor"), true);
    assert_eq!(persona.contains("Rules:"), true);
    assert_eq!(persona, FACTS_PERSONA);
}
