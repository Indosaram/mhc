use super::{
    bridge::create_recovery_bridge, derive::*, key_material::*, ports::*, recover::*,
    recovery_handles::*, salt_inputs::*, served_chain::*, testing::*, write_hold::*,
};
use crate::sync::crypto::{
    anchor::{ChainAnchor, creator_anchor},
    rejection::RejectionCounter,
};
use serde_json::json;
use std::sync::Arc;
use zeroize::Zeroizing;

#[test]
fn rejects_salt_fanout_before_authentication() {
    // Given seventeen distinct served credentials and a duplicate of the first.
    let mut rows: Vec<_> = (0..17).map(|id| json!({"body":{"kind":"recovery-key","credential_id":id.to_string(),"enrollment_nonce":"nonce"}})).collect();
    rows.push(rows[0].clone());
    // When parsing the hostile bootstrap response.
    let result = parse_salt_inputs(&json!(rows));
    // Then the authenticator input is refused, not truncated.
    assert_eq!(result, Err(SaltRefusal::TooManyRecoveryKeys));
}
#[test]
fn removed_salts_and_duplicate_enrollments_do_not_inflate_the_bound() {
    // Given seventeen enrollments with one removed credential.
    let mut rows: Vec<_> = (0..17).map(|id| json!({"body":{"kind":"recovery-key","credential_id":id.to_string(),"enrollment_nonce":"first"}})).collect();
    rows.push(json!({"body":{"kind":"recovery-key","credential_id":"0","enrollment_nonce":"replacement"}}));
    rows.push(json!({"body":{"kind":"recovery-key-removed","credential_id":"16"}}));
    // When deriving public salt inputs.
    let result = parse_salt_inputs(&json!(rows)).expect("sixteen live salts");
    // Then first occurrence and served order survive, with removals excluded.
    assert_eq!(result.len(), 16);
    assert_eq!(
        result[0],
        RecoverySaltInput {
            credential_id: "0".into(),
            enrollment_nonce: "first".into()
        }
    );
}
#[test]
fn corrupt_and_cross_account_hold_files_fail_closed() {
    // Given an atomically persisted hold, replaced by malformed bytes.
    let dir = tempfile::tempdir().expect("state directory");
    hold_writes(dir.path(), ACCOUNT, "first").expect("hold write");
    let path = std::fs::read_dir(dir.path().join("recovery-hold"))
        .expect("hold directory")
        .next()
        .expect("hold file")
        .expect("entry")
        .path();
    std::fs::write(path, b"{broken").expect("corrupt fixture");
    // When a new process reads the persisted state.
    let result = load_write_hold(dir.path(), ACCOUNT).expect("load hold");
    // Then corruption cannot restore write authority.
    assert_eq!(
        result,
        WriteHold::Held {
            credential_id: None
        }
    );
    hold_writes(dir.path(), ACCOUNT, "first").expect("restore hold");
    let path = std::fs::read_dir(dir.path().join("recovery-hold"))
        .expect("hold directory")
        .next()
        .expect("hold file")
        .expect("entry")
        .path();
    std::fs::write(path, br#"{"account":"other","credential_id":"first"}"#)
        .expect("cross account fixture");
    assert_eq!(
        load_write_hold(dir.path(), ACCOUNT).expect("hold"),
        WriteHold::Held {
            credential_id: None
        }
    );
}
fn held() -> HeldRecovery {
    HeldRecovery {
        account: ACCOUNT.into(),
        credential_id: "first".into(),
        sealing_public_key: String::new(),
        signing_public_key: String::new(),
        keys: derive_recovery_keys(&[7; 32], &[8; 32]).expect("keys"),
    }
}

#[tokio::test]
async fn acknowledged_but_missing_join_and_rotation_never_release_writes() {
    for missing_join in [true, false] {
        let dir = tempfile::tempdir().expect("state");
        let member = device(11);
        let fresh = device(12);
        let mut server = Server::new(&member, dir.path());
        let p = principal(&fresh);
        server.author = crate::sync::crypto::entry::SyncAuthor::Device {
            id: p.id,
            sealing_public_key: p.sealing_public_key,
            signing_public_key: p.signing_public_key,
        };
        server.acknowledge_join_only = missing_join;
        server.acknowledge_rotation_only = !missing_join;
        server.assert_hold_before_join = true;
        let bridge = create_recovery_bridge(Arc::new(Authenticator::enrolled()), device(12));
        let mut rejections = RejectionCounter::default();
        let outcome = recover_on_fresh_device(RecoverInput {
            account: ACCOUNT,
            rp_id: RP,
            keyring: &fresh,
            bridge: Some(&bridge),
            transport: &server,
            rejections: &mut rejections,
            state_dir: dir.path(),
            acknowledge_unknown_credential: false,
            on_event: None,
        })
        .await
        .expect("recover");
        if missing_join {
            assert_eq!(
                outcome,
                RecoverOutcome::Refused(RecoverRefusalReason::Local(
                    RecoverLocalRefusal::JoinUnconfirmed
                ))
            );
        } else {
            assert_eq!(
                outcome,
                RecoverOutcome::JoinedReadOnly {
                    credential_id: "first".into()
                }
            );
        }
        assert!(matches!(
            load_write_hold(dir.path(), ACCOUNT).expect("hold"),
            WriteHold::Held { .. }
        ));
    }
}

#[tokio::test]
async fn enrolled_key_and_last_key_confirmation_use_real_verified_membership() {
    let dir = tempfile::tempdir().expect("state");
    let member = device(11);
    let server = Server::new(&member, dir.path());
    let mut rejections = RejectionCounter::default();
    let state = load_served_chain(
        &server,
        ACCOUNT,
        &mut rejections,
        ChainAnchor::Bootstrap(creator_anchor(ACCOUNT.into(), &member)),
    )
    .await
    .expect("load")
    .expect("verified");
    let anchor = crate::sync::crypto::trust_pin::pin_trust_anchor(
        dir.path(),
        ACCOUNT,
        state.pin().expect("candidate"),
    )
    .expect("pin");
    let refused = super::remove::remove_recovery_key(super::remove::RemoveInput {
        anchor: anchor.clone(),
        account: ACCOUNT,
        rp_id: RP,
        credential_id: "first",
        confirm_giving_up_recovery: false,
        keyring: &member,
        bridge: None,
        transport: &server,
        rejections: &mut rejections,
        on_event: None,
    })
    .await
    .expect("removal refusal");
    assert_eq!(
        refused,
        super::remove::RemoveOutcome::Refused(super::remove::RemoveRefusal::Local(
            super::remove::RemoveLocalRefusal::ConfirmLastKey
        ))
    );
    let bridge = create_recovery_bridge(
        Arc::new(Authenticator {
            credential: "second".into(),
            supported: true,
        }),
        device(11),
    );
    let enrolled = super::enroll::enroll_recovery_key(super::enroll::EnrollInput {
        anchor: anchor.clone(),
        account: ACCOUNT,
        rp_id: RP,
        user_name: "test",
        roaming: false,
        keyring: &member,
        bridge: &bridge,
        transport: &server,
        rejections: &mut rejections,
        on_event: None,
    })
    .await
    .expect("enroll");
    assert_eq!(
        enrolled,
        super::enroll::EnrollOutcome::Enrolled {
            credential_id: "second".into(),
            seq: 2
        }
    );
    let state = load_served_chain(
        &server,
        ACCOUNT,
        &mut rejections,
        ChainAnchor::Pinned(anchor),
    )
    .await
    .expect("reload")
    .expect("verified enrollment");
    assert_eq!(state.authority().expect("authority").registry.len(), 2);
}
#[tokio::test(start_paused = true)]
async fn handles_are_one_shot_and_second_begin_retires_old_keys() {
    // Given two consecutive holds in one bridge.
    let store = RecoveryHandles::default();
    let old = store.hold(held()).expect("old handle");
    let current = store.hold(held()).expect("current handle");
    // When consuming both handles twice.
    let results = [
        store.take(&old).is_some(),
        store.take(&current).is_some(),
        store.take(&current).is_some(),
    ];
    // Then only the newest handle succeeds once.
    assert_eq!(results, [false, true, false]);
}
#[tokio::test(start_paused = true)]
async fn active_expiry_disposes_without_a_take_call() {
    // Given a held secret and its active expiry timer.
    let store = RecoveryHandles::default();
    store.hold(held()).expect("handle");
    let expired = super::recovery_handles::expiry_signal(&store);
    // When virtual time reaches the timer deadline.
    tokio::time::advance(tokio::time::Duration::from_millis(RECOVERY_HANDLE_TTL_MS)).await;
    tokio::time::timeout(tokio::time::Duration::from_secs(1), expired)
        .await
        .expect("expiry event");
    // Then no keys remain held even without consumption.
    assert!(!super::recovery_handles::has_live(&store));
}
#[test]
fn derivation_is_nfc_normalized_and_domain_separated() {
    // Given canonically equivalent account names and a fixed nonce.
    let nonce = [8; 32];
    // When deriving WebAuthn salt and user handle.
    let salt = recovery_prf_salt("e\u{301}", &nonce).expect("salt");
    // Then NFC is stable while the two purposes remain distinct.
    assert_eq!(salt, recovery_prf_salt("\u{e9}", &nonce).expect("salt"));
    assert_eq!(
        recovery_user_handle("e\u{301}"),
        recovery_user_handle("\u{e9}")
    );
    assert_ne!(salt, recovery_user_handle("\u{e9}"));
}
#[test]
fn decryptable_substituted_recovery_wrap_is_not_enrollment_proof() {
    // Given two decryptable wraps to the same credential at the same epoch.
    let keys = derive_recovery_keys(&[7; 32], &[8; 32]).expect("keys");
    let public = public_key_text(&keys).0;
    let address = WrapAddress {
        account: ACCOUNT,
        epoch: 1,
        id: "recovery:first",
    };
    let original = EpochMaterial {
        epoch: 1,
        current: Zeroizing::new(vec![9; 32]),
        history: Vec::new(),
    };
    let substitute = EpochMaterial {
        epoch: 1,
        current: Zeroizing::new(vec![10; 32]),
        history: Vec::new(),
    };
    let wrap = seal_wrap(&address, &public, &original).expect("enrollment wrap");
    let other = seal_wrap(&address, &public, &substitute).expect("substitute wrap");
    let key = crate::sync::crypto::parse::RecoveryKey {
        credential_id: "first".into(),
        rp_id: RP.into(),
        enrollment_nonce: String::new(),
        sealing_public_key: public,
        signing_public_key: public_key_text(&keys).1,
        survivable: crate::sync::crypto::parse::Survivable::Synced,
    };
    let candidate = RecoveryCandidate {
        key,
        countersign: Countersign {
            custody_root: String::new(),
            predecessor: String::new(),
            wraps_digest: crate::sync::crypto::links::wrap_set_digest(std::slice::from_ref(&wrap))
                .expect("digest"),
            signature: String::new(),
        },
        wrap: other,
        expected_history: Default::default(),
        enrollment: Enrollment {
            wraps: vec![wrap],
            expected_history: Default::default(),
        },
    };
    // When checking provenance rather than just decryption.
    let result = super::bridge_provenance::proven_recovery_material(ACCOUNT, &candidate, &keys);
    // Then a substituted same-epoch wrap is rejected.
    assert!(matches!(result, Err(RecoverRefusal::WrapUnbound)));
}
#[tokio::test]
async fn withheld_rotation_leaves_a_successfully_joined_device_persistently_read_only() {
    // Given a valid encrypted chain whose server withholds rotation.
    let dir = tempfile::tempdir().expect("state");
    let member = device(11);
    let fresh = device(12);
    let mut server = Server::new(&member, dir.path());
    server.author = {
        let p = principal(&fresh);
        crate::sync::crypto::entry::SyncAuthor::Device {
            id: p.id,
            sealing_public_key: p.sealing_public_key,
            signing_public_key: p.signing_public_key,
        }
    };
    server.refuse_rotation = true;
    server.assert_hold_before_join = true;
    let bridge = create_recovery_bridge(Arc::new(Authenticator::enrolled()), device(12));
    let mut rejections = RejectionCounter::default();
    // When the fresh device recovers through the real bridge and chain verifier.
    let result = recover_on_fresh_device(RecoverInput {
        account: ACCOUNT,
        rp_id: RP,
        keyring: &fresh,
        bridge: Some(&bridge),
        transport: &server,
        rejections: &mut rejections,
        state_dir: dir.path(),
        acknowledge_unknown_credential: false,
        on_event: None,
    })
    .await
    .expect("recovery operation");
    // Then the join landed, but writes remain held across reload.
    assert_eq!(
        result,
        RecoverOutcome::JoinedReadOnly {
            credential_id: "first".into()
        }
    );
    assert_eq!(server.rows.lock().expect("rows").len(), 2);
    assert_eq!(
        load_write_hold(dir.path(), ACCOUNT).expect("hold"),
        WriteHold::Held {
            credential_id: Some("first".into())
        }
    );
}
#[tokio::test]
async fn verified_rotation_releases_the_hold() {
    // Given a complete valid chain and a fresh recovery device.
    let dir = tempfile::tempdir().expect("state");
    let member = device(11);
    let fresh = device(12);
    let mut server = Server::new(&member, dir.path());
    let p = principal(&fresh);
    server.author = crate::sync::crypto::entry::SyncAuthor::Device {
        id: p.id,
        sealing_public_key: p.sealing_public_key,
        signing_public_key: p.signing_public_key,
    };
    server.assert_hold_before_join = true;
    let bridge = create_recovery_bridge(Arc::new(Authenticator::enrolled()), device(12));
    let mut rejections = RejectionCounter::default();
    // When recovery joins and reloads a stored rotation.
    let result = recover_on_fresh_device(RecoverInput {
        account: ACCOUNT,
        rp_id: RP,
        keyring: &fresh,
        bridge: Some(&bridge),
        transport: &server,
        rejections: &mut rejections,
        state_dir: dir.path(),
        acknowledge_unknown_credential: false,
        on_event: None,
    })
    .await
    .expect("recovery operation");
    // Then only the fresh epoch grants write authority.
    assert_eq!(
        result,
        RecoverOutcome::Recovered {
            epoch: 2,
            credential_id: "first".into()
        }
    );
    assert_eq!(
        load_write_hold(dir.path(), ACCOUNT).expect("hold"),
        WriteHold::Released
    );
}
#[tokio::test]
async fn removal_pending_resumes_rotation_without_a_second_removal() {
    // Given a verified member pin and a server refusing the first rotation.
    let dir = tempfile::tempdir().expect("state");
    let member = device(11);
    let mut server = Server::new(&member, dir.path());
    server.refuse_rotation = true;
    let mut rejections = RejectionCounter::default();
    let state = load_served_chain(
        &server,
        ACCOUNT,
        &mut rejections,
        ChainAnchor::Bootstrap(creator_anchor(ACCOUNT.into(), &member)),
    )
    .await
    .expect("load")
    .expect("verified");
    let anchor = crate::sync::crypto::trust_pin::pin_trust_anchor(
        dir.path(),
        ACCOUNT,
        state.pin().expect("pin candidate"),
    )
    .expect("pin");
    let outcome = super::remove::remove_recovery_key(super::remove::RemoveInput {
        anchor: anchor.clone(),
        account: ACCOUNT,
        rp_id: RP,
        credential_id: "first",
        confirm_giving_up_recovery: true,
        keyring: &member,
        bridge: None,
        transport: &server,
        rejections: &mut rejections,
        on_event: None,
    })
    .await
    .expect("remove");
    assert_eq!(
        outcome,
        super::remove::RemoveOutcome::RotationPending {
            platform_signal: PlatformSignal::Unsupported,
            delete_from_passkey_manager: true
        }
    );
    server.refuse_rotation = false;
    // When retrying after removal already landed.
    let result = super::remove::remove_recovery_key(super::remove::RemoveInput {
        anchor,
        account: ACCOUNT,
        rp_id: RP,
        credential_id: "first",
        confirm_giving_up_recovery: false,
        keyring: &member,
        bridge: None,
        transport: &server,
        rejections: &mut rejections,
        on_event: None,
    })
    .await
    .expect("retry removal");
    // Then rotation completes, preserving unsupported-platform user action.
    assert_eq!(
        result,
        super::remove::RemoveOutcome::Removed {
            epoch: 2,
            platform_signal: PlatformSignal::Unsupported,
            delete_from_passkey_manager: true
        }
    );
    assert_eq!(
        server
            .rows
            .lock()
            .expect("rows")
            .iter()
            .filter(|row| row["body"]["kind"] == "recovery-key-removed")
            .count(),
        1
    );
}
