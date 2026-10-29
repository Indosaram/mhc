use pretty_assertions::assert_eq;
use std::path::PathBuf;

use super::*;

#[test]
fn test_load_facts_persona_non_empty_and_valid() {
    let persona = load_facts_persona();
    assert_eq!(persona.is_empty(), false);
    assert_eq!(persona.starts_with("# Facts extractor"), true);
    assert_eq!(persona.contains("record_fact"), true);
    assert_eq!(persona, FACTS_PERSONA);
}

#[test]
fn test_load_facts_persona_from_dir() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("facts");
    let loaded = load_facts_persona_from_dir(&dir).expect("the facts persona loads");
    assert_eq!(loaded, FACTS_PERSONA);
}

#[test]
fn test_load_facts_persona_from_missing_dir() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("facts")
        .join("missing-dir");
    assert!(load_facts_persona_from_dir(&dir).is_err());
}
