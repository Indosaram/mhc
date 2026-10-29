use maho_omo_onboarding::state::{claim_onboarding, is_onboarding_complete, release_onboarding};

#[test]
fn release_removes_claimed_marker_and_is_idempotent() {
    let state = tempfile::tempdir().expect("private onboarding state");
    assert!(claim_onboarding(state.path()));
    assert!(is_onboarding_complete(state.path()));

    release_onboarding(state.path()).expect("release existing marker");
    assert!(!is_onboarding_complete(state.path()));
    release_onboarding(state.path()).expect("missing marker is already released");
}

#[test]
fn release_preserves_unrelated_state_files() {
    let state = tempfile::tempdir().expect("private onboarding state");
    std::fs::write(state.path().join("onboarding-declined-global"), "declined").expect("decline marker");
    release_onboarding(state.path()).expect("release missing onboarding marker");
    assert!(state.path().join("onboarding-declined-global").exists());
}
