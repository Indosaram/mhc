use super::{testing::*, *};
use crate::sync::keyring_port::DeviceKeyring;
use serde_json::json;

#[test]
fn folds_golden_chain_and_mints_wraps_only_after_anchor() {
    // Given the upstream chain with recovery, veto and rotation.
    let f = fixtures();
    let rows = main_chain(&f);
    let mut counter = create_rejection_counter(None);
    // When admitting the chain under the creator's own key.
    let outcome = append_entries(&start(&f), &rows, &mut counter);
    // Then membership and epoch come from the verified fold.
    let AppendOutcome::Accepted { state, steps } = outcome else {
        panic!("{outcome:?}")
    };
    let a = state.authority().expect("authority");
    assert_eq!(a.epoch, 3);
    assert_eq!(a.members.len(), 2);
    assert_eq!(steps.len(), 8);
    assert!(state.received().iter().any(|e| !e.wraps.is_empty()));
    assert_eq!(counter.count(), 0);
}
#[test]
fn unanchored_prefix_returns_original_cursor_and_no_wraps() {
    // Given a valid genesis but a recovery ceremony for a different credential.
    let f = fixtures();
    let anchor = recovery_anchor(
        "encoding-account".into(),
        "missing".into(),
        PublicKeys {
            sealing_public_key: "s".into(),
            signing_public_key: "k".into(),
        },
    );
    let start = empty_chain("encoding-account".into(), ChainAnchor::Bootstrap(anchor));
    // When only that genesis is walked.
    let result = append_entries(
        &start,
        &[row(&f, "genesis")],
        &mut create_rejection_counter(None),
    );
    // Then no accepted position or verified wraps escape the unanchored walk.
    let AppendOutcome::Rejected {
        state,
        steps,
        reason,
        ..
    } = result
    else {
        panic!("{result:?}")
    };
    assert_eq!(reason, "unanchored");
    assert!(state.head().is_none());
    assert!(state.received().is_empty());
    assert!(steps.is_empty());
}
#[test]
fn anchored_walk_stops_at_first_failure_and_keeps_prefix() {
    // Given valid genesis followed by an invalid link and another valid row.
    let f = fixtures();
    let mut bad = row(&f, "recovery_key");
    bad["link"] = "bad".into();
    let mut counter = create_rejection_counter(None);
    // When admitting the sequence.
    let result = append_entries(
        &start(&f),
        &[row(&f, "genesis"), bad, row(&f, "recovery_key_removed")],
        &mut counter,
    );
    // Then only genesis is committed and only one rejection is recorded.
    let AppendOutcome::Rejected {
        state,
        steps,
        reason,
        seq,
    } = result
    else {
        panic!("{result:?}")
    };
    assert_eq!(reason, "link_mismatch");
    assert_eq!(seq, 2);
    assert_eq!(state.head().expect("head").seq, 1);
    assert_eq!(steps.len(), 1);
    assert_eq!(counter.count(), 1);
}
#[test]
fn transport_refuses_batch_body_before_downstream_access() {
    // Given an otherwise malformed batch carrying a body.
    let row = json!({"kind":"batch","body":null});
    // When parsing the closed transport boundary.
    let result = envelope::parse_entry(&row);
    // Then the receiver's special batch-body refusal is preserved.
    assert!(matches!(result, Err("batch_with_body")));
}

#[test]
fn fork_receiver_recomputes_state_and_checks_evidence() {
    let principal =
        json!({"kind":"device","id":"a","sealing_public_key":"s","signing_public_key":"k"});
    let genesis = json!({"seq":1,"kind":"keyring","digest":"root","author":principal,"body":{"kind":"genesis","bootstrap":{"first_device":principal,"recovery_key":{"credential_id":"c"}}}});
    let branch = |digest: &str| json!({"tip":{"seq":2,"digest":digest,"keyring_link":"root-link"},"records":[{"seq":2,"base_seq":1,"epoch":1,"kind":"batch","digest":digest,"keyring_link":null,"author":principal}]});
    let branches = json!([branch("a"), branch("b")]);
    let mut resolution = json!({"seq":3,"kind":"keyring","author":principal,"body":{"kind":"fork-resolution","fork_set":[branches[0]["tip"],branches[1]["tip"]],
        "evidence":{"common":{"seq":1,"digest":"root","keyring_link":"root-link"},"branches":branches},
        "state":{"members":[],"recovery_keys":[],"epoch":1}}});
    assert!(matches!(
        vendor::sync_format::fold::fold_refusal(&[genesis.clone(), resolution.clone()]),
        Err((3, "fork_state_mismatch"))
    ));
    resolution["body"]["state"] =
        json!({"members":[principal],"recovery_keys":[{"credential_id":"c"}],"epoch":1});
    assert!(
        vendor::sync_format::fold::fold_refusal(&[genesis.clone(), resolution.clone()]).is_ok()
    );
    assert!(matches!(
        vendor::sync_format::receive::receive_keyring_chain(&[genesis, resolution]),
        Err((3, "fork_evidence_invalid"))
    ));
}

#[test]
fn fork_resolution_requires_the_named_following_rotation() {
    use chain_checks::forced_rotation_refusal;
    assert_eq!(
        forced_rotation_refusal(Some(7), None),
        Some("forced_rotation_required")
    );
    let body = parse::ParsedBody::Rotation {
        members: Vec::new(),
        wraps_digest: String::new(),
        fork_resolution: Some(7),
    };
    assert_eq!(forced_rotation_refusal(Some(7), Some(&body)), None);
    assert_eq!(
        forced_rotation_refusal(None, Some(&body)),
        Some("bad_fork_resolution")
    );
}
#[test]
fn vetoed_batch_advances_cursor_without_applying_content() {
    // Given a chain that cancelled epoch 2 and a batch signed by that epoch's author.
    let f = fixtures();
    let state = accepted(&f);
    let author = keyring(&f, "recovered");
    let head = state.head().expect("head");
    let digest = signatures::sha256_b64(b"cancelled batch");
    let fields = SyncRecordFields {
        account: "encoding-account".into(),
        base_seq: head.seq,
        digest: digest.clone(),
        epoch: 2,
        link: next_link(&head.link, &digest).expect("link"),
        keyring_link: None,
    };
    let entry = SyncEntry::Batch(BatchEntry {
        seq: head.seq + 1,
        author: SyncAuthor::Device {
            id: author.principal().id,
            sealing_public_key: author.public_keys().sealing,
            signing_public_key: author.public_keys().signing,
        },
        record_sig: sign_record(&author, &fields).expect("sign"),
        fields,
    });
    // When the authenticated cancelled batch arrives.
    let result = append_entries(&state, &[entry.wire()], &mut create_rejection_counter(None));
    // Then the cursor advances but the step carries no batch content.
    let AppendOutcome::Accepted { state: next, steps } = result else {
        panic!("{result:?}")
    };
    assert!(matches!(steps.as_slice(), [ChainStep::Vetoed { .. }]));
    assert_eq!(next.head().expect("next").seq, head.seq + 1);
}
