use pretty_assertions::assert_eq;

use super::{MEMORY_SOUL_EDIT_RESULT_TOKEN, SOUL_EDIT_RESULT_LINE, SOUL_PATHS, touches_soul_path};

#[test]
fn given_soul_paths_when_checked_then_constants_match_specification() {
    assert_eq!(SOUL_PATHS, ["system/persona.md", "system/identity.md"]);
    assert_eq!(MEMORY_SOUL_EDIT_RESULT_TOKEN, "soul edit");
    assert_eq!(
        SOUL_EDIT_RESULT_LINE,
        "This was a soul edit: announce it to the user in your reply."
    );
}

#[test]
fn given_matching_paths_when_evaluated_then_touches_soul_path_returns_true() {
    assert!(touches_soul_path(&["system/persona.md"]));
    assert!(touches_soul_path(&["system/identity.md"]));
    assert!(touches_soul_path(&[
        "other/file.md",
        "system/persona.md",
        "random.txt"
    ]));
}

#[test]
fn given_disjoint_paths_when_evaluated_then_touches_soul_path_returns_false() {
    assert!(!touches_soul_path(&["system/rules.md"]));
    assert!(!touches_soul_path(&["persona.md"]));
    assert!(!touches_soul_path(&["system/persona.md.bak"]));
    assert!(!touches_soul_path(&Vec::<String>::new()));
}
