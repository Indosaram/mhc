use super::*;

#[test]
fn given_two_identities_in_one_scheme_when_they_disagree_then_the_conflict_is_reported() {
    assert!(start_identities_conflict("proc-start-epoch:1789041850", "proc-start-epoch:1789041999"));
    assert!(start_identities_conflict("ps-lstart:Thu Sep 10 21:04:10 2026", "ps-lstart:Thu Sep 10 22:04:10 2026"));
}

#[test]
fn given_identities_in_different_schemes_when_compared_then_no_conflict_is_reported() {
    assert!(!start_identities_conflict("ps-lstart:Thu Sep 10 21:04:10 2026", "proc-start-epoch:1789041850"));
    assert!(!start_identities_conflict("proc-start-epoch:1789041850", "linux-proc-start-ticks:912"));
}

#[test]
fn given_an_identity_with_no_scheme_separator_when_compared_then_no_conflict_is_reported() {
    assert!(!start_identities_conflict("unavailable", "proc-start-epoch:1789041850"));
    assert!(!start_identities_conflict(":1789041850", "proc-start-epoch:1789041850"));
}

#[test]
fn given_identical_identities_when_compared_then_no_conflict_is_reported() {
    assert!(!start_identities_conflict("proc-start-epoch:1789041850", "proc-start-epoch:1789041850"));
}

#[test]
fn comparability_requires_matching_schemes_including_the_schemeless_case() {
    assert!(start_identities_comparable("proc-start-epoch:1", "proc-start-epoch:2"));
    assert!(!start_identities_comparable("proc-start-epoch:1", "linux-proc-start-ticks:2"));
    assert!(start_identities_comparable("legacy", "other"));
    assert!(!start_identities_comparable("legacy", "proc-start-epoch:1"));
}
