//! Regression cases ported from the session-move suite, including every durable boundary.
use super::*;
use bundle::{MoveManifest, RepositoryIdentity};
use move_ledger::{MoveLedger, MoveStatus};
use move_steps::{MoveStep, with_move_step_hook};
use serde_json::json;
use std::path::{Path, PathBuf};
fn identity(_: &Path) -> Result<Option<RepositoryIdentity>, String> {
    Ok(Some(RepositoryIdentity {
        root_commits: vec!["a".repeat(40)],
        origin_url: None,
    }))
}
struct Fixture {
    _root: tempfile::TempDir,
    sender: PathBuf,
    bundle: PathBuf,
    receiver: PathBuf,
    ledger: MoveLedger,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let sender = root.path().join("sender/session.jsonl");
        std::fs::create_dir_all(sender.parent().expect("parent")).expect("sender dir");
        std::fs::write(
            &sender,
            format!("{}\n", json!({"type":"session","id":"s1","cwd":"/old"})),
        )
        .expect("session");
        append_custom(
            &sender,
            "repository-identity",
            json!({"rootCommits":["a".repeat(40)]}),
        )
        .expect("identity");
        let bundle = root.path().join("bundle");
        let receiver = root.path().join("receiver");
        let ledger = MoveLedger::new(root.path().join("ledger"));
        let fixture = Self {
            _root: root,
            sender,
            bundle,
            receiver,
            ledger,
        };
        fixture.export(None).await.expect("export");
        fixture.ledger.prepare("m1").await.expect("ready");
        fixture
    }
    async fn export(&self, state: Option<&Path>) -> Result<MoveManifest, String> {
        export::export_session_for_move(export::ExportSessionForMoveOptions {
            session_file: &self.sender,
            move_id: "m1",
            target: "receiver",
            dropped_user_messages: &["hello".into()],
            bundle_dir: &self.bundle,
            extension_state_dir: state,
            resolve_extension_state: None,
            guard: None,
            locked_guard: None,
        })
        .await
    }
    async fn import(&self) -> Result<PathBuf, String> {
        import::import_moved_session(import::ImportMovedSessionOptions {
            bundle_dir: &self.bundle,
            target_cwd: &self.receiver,
            ledger: &self.ledger,
            session_dir: Some(&self.receiver),
            from: Some("sender"),
            read_identity: Some(&identity),
        })
        .await
    }
    async fn finish(&self, answer: &MoveStatus) -> Result<(), String> {
        finish::finish_move(finish::FinishMoveOptions {
            session_file: &self.sender,
            bundle_dir: &self.bundle,
            move_id: "m1",
            answer,
            guard: None,
        })
        .await
    }
    async fn revoke(&self, file: &Path) -> Result<(), String> {
        revoke::revoke_imported_move(revoke::RevokeMoveOptions {
            session_file: file,
            move_id: "m1",
            ledger: &self.ledger,
            guard: None,
        })
        .await
    }
}
#[tokio::test]
async fn import_fault_boundaries_reconcile_from_file_and_never_land_twice() {
    for boundary in [
        MoveStep::ImportReceived,
        MoveStep::ImportStamped,
        MoveStep::ImportRenamed,
        MoveStep::ImportAccepted,
    ] {
        let f = Fixture::new().await;
        let failed = with_move_step_hook(
            std::sync::Arc::new(move |step| {
                if step == boundary {
                    Err("injected".into())
                } else {
                    Ok(())
                }
            }),
            f.import(),
        )
        .await;
        assert!(failed.is_err(), "{boundary:?}");
        let status = import::answer_move_status(&f.ledger, "m1", None)
            .await
            .expect("status");
        match boundary {
            MoveStep::ImportReceived => {
                assert_eq!(status, MoveStatus::Receiving);
                f.import().await.expect("retry");
            }
            MoveStep::ImportStamped => {
                assert!(matches!(status, MoveStatus::Failed { .. }));
                assert!(f.import().await.is_err());
            }
            _ => {
                assert_eq!(status, MoveStatus::Accepted);
                f.import().await.expect("same landed file");
            }
        }
        let other = f.receiver.join("other");
        assert!(
            import::import_moved_session(import::ImportMovedSessionOptions {
                bundle_dir: &f.bundle,
                target_cwd: &other,
                ledger: &f.ledger,
                session_dir: Some(&other),
                from: None,
                read_identity: Some(&identity)
            })
            .await
            .is_err()
        );
        assert_ne!(
            ownership::session_ownership(&entries(&f.sender)),
            ownership::SessionOwnership::Writable
        );
    }
}
#[tokio::test]
async fn revoke_fault_boundaries_preserve_the_mark_and_reclaim_sender() {
    for boundary in [MoveStep::RevokeSealed, MoveStep::RevokeMarked] {
        let f = Fixture::new().await;
        let imported = f.import().await.expect("import");
        assert!(
            with_move_step_hook(
                std::sync::Arc::new(move |step| if step == boundary {
                    Err("injected".into())
                } else {
                    Ok(())
                }),
                f.revoke(&imported)
            )
            .await
            .is_err()
        );
        let answer = import::answer_move_status(&f.ledger, "m1", None)
            .await
            .expect("status");
        assert_eq!(answer, MoveStatus::AcceptedRevoked);
        f.revoke(&imported).await.expect("idempotent revoke");
        f.finish(&answer).await.expect("reclaim");
        assert_eq!(
            ownership::session_ownership(&entries(&f.sender)),
            ownership::SessionOwnership::Writable
        );
        assert_ne!(
            ownership::session_ownership(&entries(&imported)),
            ownership::SessionOwnership::Writable
        );
    }
}
#[tokio::test]
async fn finish_fault_boundaries_requeue_exactly_once() {
    for boundary in [MoveStep::FinishRecorded, MoveStep::FinishRequeued] {
        let f = Fixture::new().await;
        let answer = MoveStatus::Failed {
            reason: move_ledger::FailReason::ImportFailed,
        };
        assert!(
            with_move_step_hook(
                std::sync::Arc::new(move |step| if step == boundary {
                    Err("injected".into())
                } else {
                    Ok(())
                }),
                f.finish(&answer)
            )
            .await
            .is_err()
        );
        f.finish(&answer).await.expect("retry finish");
        f.finish(&answer).await.expect("repeat finish");
        assert!(!f.bundle.exists());
        assert_eq!(
            entries(&f.sender)
                .iter()
                .filter(|e| e["customType"] == "pending_input_transition"
                    && e["data"]["input"]["clientMessageId"] == "move:m1:0")
                .count(),
            1
        );
    }
}
#[tokio::test]
async fn payloads_are_verified_staged_and_redelivered_on_fresh_attempt_copies() {
    let f = Fixture::new().await;
    let state = f._root.path().join("state");
    std::fs::create_dir_all(state.join("probe")).expect("state");
    std::fs::write(state.join("probe/state.json"), "original").expect("write");
    let manifest = f.export(Some(&state)).await.expect("export state");
    assert_eq!(manifest.payloads[0].files[0].path, "state/probe/state.json");
    let imported = f.import().await.expect("import");
    let mut stale = PathBuf::new();
    extension_state::restore_moved_state(&imported, |event| {
        stale = event.dir.clone();
        async move {
            assert!(!event.redelivered);
            std::fs::write(event.dir.join("probe/state.json"), "mutated").expect("mutate");
            Ok(false)
        }
    })
    .await
    .expect("failed delivery");
    std::fs::create_dir_all(stale.join("probe")).expect("late attempt");
    std::fs::write(stale.join("probe/state.json"), "late").expect("late write");
    extension_state::restore_moved_state(&imported, |event| async move {
        assert!(event.redelivered);
        assert_eq!(
            std::fs::read_to_string(event.dir.join("probe/state.json")).expect("read"),
            "original"
        );
        Ok(true)
    })
    .await
    .expect("restore");
    assert!(
        extension_state::pending_moved_state(&imported)
            .expect("pending")
            .is_none()
    );
}
#[tokio::test]
async fn revoked_answer_cannot_reopen_an_already_moved_sender_and_wrong_status_path_is_refused() {
    let f = Fixture::new().await;
    let imported = f.import().await.expect("import");
    assert!(
        import::answer_move_status(&f.ledger, "m1", Some(&f.sender))
            .await
            .is_err()
    );
    f.finish(&MoveStatus::Accepted).await.expect("accepted");
    f.revoke(&imported).await.expect("revoke");
    assert!(
        f.finish(&MoveStatus::AcceptedRevoked)
            .await
            .expect_err("moved")
            .contains("move_already_moved")
    );
}
#[test]
fn sealed_attempt_is_the_only_attempt_selected_and_payload_tamper_is_refused() {
    let root = tempfile::tempdir().expect("tempdir");
    let file = root.path().join("s.jsonl");
    std::fs::write(&file, "{\"type\":\"session\",\"id\":\"s\"}\n").expect("session");
    let first = extension_state::prepare_outgoing_state(&file, "m1").expect("first");
    let second = extension_state::prepare_outgoing_state(&file, "m1").expect("second");
    assert!(!first.dir.exists());
    append_custom(
        &file,
        ownership::SESSION_RELEASED,
        json!({"move_id":"m1","state_attempt":second.attempt}),
    )
    .expect("seal");
    assert_eq!(
        extension_state::sealed_outgoing_state_dir(&file, "m1").expect("sealed"),
        Some(second.dir)
    );
}

#[tokio::test]
async fn payload_changed_after_verification_never_reaches_receiver_state() {
    let f = Fixture::new().await;
    let state = f._root.path().join("state");
    std::fs::create_dir_all(&state).expect("state");
    std::fs::write(state.join("data"), "original").expect("write");
    f.export(Some(&state)).await.expect("export");
    let mutate = |path: &Path| {
        std::fs::write(f.bundle.join("state/data"), "tampered").expect("tamper");
        identity(path)
    };
    let result = import::import_moved_session(import::ImportMovedSessionOptions {
        bundle_dir: &f.bundle,
        target_cwd: &f.receiver,
        ledger: &f.ledger,
        session_dir: Some(&f.receiver),
        from: None,
        read_identity: Some(&mutate),
    })
    .await;
    assert!(result.expect_err("tampered").contains("changed in transit"));
    assert!(
        !extension_state::moved_state_dir(&f.receiver.join("session.jsonl"), "m1")
            .expect("path")
            .exists()
    );
    assert_eq!(
        f.ledger.answer_status("m1", false).await.expect("status"),
        MoveStatus::Failed {
            reason: move_ledger::FailReason::BundleInvalid
        }
    );
}
#[tokio::test]
async fn live_holder_fences_export_and_move_fences_holder_admission() {
    let f = Fixture::new().await;
    let hold = crate::schedule::runner_lease::hold_session_file(&f.sender, "s1", "/old", true)
        .expect("hold");
    assert!(
        f.export(None)
            .await
            .expect_err("held")
            .contains("ESESSIONHELD")
    );
    drop(hold);
    session_lock::with_session_move_lock(
        &f.sender,
        async {
            assert!(
                crate::schedule::runner_lease::hold_session_file(&f.sender, "s1", "/old", true)
                    .is_err()
            );
            Ok(())
        },
        None,
    )
    .await
    .expect("move lock");
}
#[tokio::test]
async fn status_refuses_non_import_file_but_missing_path_uses_ledger() {
    let f = Fixture::new().await;
    assert!(
        import::answer_move_status(&f.ledger, "m1", Some(&f.sender))
            .await
            .is_err()
    );
    assert_eq!(
        import::answer_move_status(&f.ledger, "m1", Some(&f.receiver.join("missing")))
            .await
            .expect("missing"),
        MoveStatus::Receiving
    );
    assert_eq!(
        f.ledger
            .answer_status("m1", false)
            .await
            .expect("unchanged"),
        MoveStatus::Receiving
    );
}
#[test]
fn payload_parser_checks_kind_prefix_and_traversal() {
    let root = tempfile::tempdir().expect("tempdir");
    let base = json!({"format":1,"moveId":"m1","sessionId":"s1","sessionFileName":"s1.jsonl","sourceCwd":"/x","session":{"path":"files/session.jsonl","sha256":"x"}});
    for (kind, path) in [
        ("unknown", "state/x"),
        ("extension-state", "files/x"),
        ("extension-state", "state/../x"),
        ("extension-state", "state/..\\x"),
    ] {
        let mut manifest = base.clone();
        manifest["payloads"] = json!([{"kind":kind,"files":[{"path":path,"sha256":"x"}]}]);
        std::fs::write(
            root.path().join(bundle::MANIFEST_FILE),
            manifest.to_string(),
        )
        .expect("manifest");
        assert!(bundle::read_manifest(root.path()).is_err());
    }
    let mut manifest = base;
    manifest["payloads"] =
        json!([{"kind":"extension-state","files":[{"path":"state/probe/data","sha256":"x"}]}]);
    std::fs::write(
        root.path().join(bundle::MANIFEST_FILE),
        manifest.to_string(),
    )
    .expect("manifest");
    assert_eq!(
        bundle::read_manifest(root.path())
            .expect("known payload")
            .payloads
            .len(),
        1
    );
}

#[tokio::test]
async fn sealed_release_payload_is_discovered_without_a_caller_override() {
    let f = Fixture::new().await;
    let staged = extension_state::prepare_outgoing_state(&f.sender, "m1").expect("stage");
    std::fs::write(staged.dir.join("extension.json"), "state").expect("state");
    append_custom(
        &f.sender,
        ownership::SESSION_RELEASED,
        json!({"reason":"move","move_id":"m1","state_attempt":staged.attempt}),
    )
    .expect("seal");
    let manifest = f.export(None).await.expect("export sealed state");
    assert!(
        manifest
            .payloads
            .iter()
            .flat_map(|payload| payload.files.iter())
            .any(|entry| entry.path.contains("extension.json"))
    );
}
