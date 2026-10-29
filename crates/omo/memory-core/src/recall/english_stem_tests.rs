use super::*;

#[test]
fn short_and_non_ascii_tokens_pass_through_untouched() {
    assert_eq!(stem_english_token("a"), "a");
    assert_eq!(stem_english_token("the"), "the");
    assert_eq!(stem_english_token("abc"), "abc");
    assert_eq!(stem_english_token("리콜"), "리콜");
    assert_eq!(stem_english_token("Version2"), "Version2");
}

#[test]
fn plurals_fold_onto_their_singular() {
    assert_eq!(stem_english_token("rollbacks"), "rollback");
    assert_eq!(stem_english_token("deploys"), "deploy");
    assert_eq!(stem_english_token("policies"), "policy");
    assert_eq!(stem_english_token("classes"), "class");
    assert_eq!(stem_english_token("boxes"), "box");
    assert_eq!(stem_english_token("dishes"), "dish");
    assert_eq!(stem_english_token("buses"), "bus");
}

#[test]
fn ing_and_ed_fold_and_collapse_a_doubled_consonant() {
    assert_eq!(stem_english_token("running"), "run");
    assert_eq!(stem_english_token("stopped"), "stop");
    assert_eq!(stem_english_token("killing"), "kill");
    assert_eq!(stem_english_token("passed"), "pass");
    assert_eq!(stem_english_token("rotated"), "rotat");
}

#[test]
fn derivational_suffixes_fold() {
    assert_eq!(stem_english_token("deployment"), "deploy");
    assert_eq!(stem_english_token("quickly"), "quick");
    assert_eq!(stem_english_token("rotation"), "rotat");
    assert_eq!(stem_english_token("deployment"), stem_english_token("deploys"));
}

#[test]
fn a_fold_never_returns_a_word_shorter_than_its_own_rule_allows() {
    assert_eq!(stem_english_token("being"), "being");
    assert_eq!(stem_english_token("sing"), "sing");
    assert_eq!(stem_english_token("deed"), "deed");
}
