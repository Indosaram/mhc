//! `team/member-extension/self-poller.test.ts`

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::config::TeamModeConfig;
use team_core::team_mailbox::{commit_delivery_reservation, reserve_message_for_delivery};
use team_core::types::Message;

use crate::team::member_extension::qa_inject_hold::QaAfterInjectHold;
use crate::team::member_extension::self_poller::{
    MemberSelfPoller, MemberSelfPollerDeps, TeamMessageEvent, create_member_self_poller,
};
use crate::team::messaging::message::build_peer_message_envelope;

const TEAM_RUN_ID: &str = "11111111-1111-4111-8111-111111111111";

/// What the inject-hold probe records: the injected message ids, whether the delivery reservation
/// was still uncommitted inside the hold, and whether it was committed once released.
type ObservedInHold = Arc<Mutex<Option<(Vec<String>, bool, bool)>>>;

struct Harness {
    _root: tempfile::TempDir,
    config: TeamModeConfig,
    inbox_dir: PathBuf,
    session_dir: PathBuf,
    injected: Arc<Mutex<Vec<String>>>,
    events: Arc<Mutex<Vec<TeamMessageEvent>>>,
}

impl Harness {
    fn injected(&self) -> Vec<String> {
        self.injected.lock().unwrap().clone()
    }

    fn events(&self) -> Vec<TeamMessageEvent> {
        self.events.lock().unwrap().clone()
    }
}

fn create_harness() -> Harness {
    let root = tempfile::tempdir().unwrap();
    let base_dir = root.path().join("teams");
    let session_dir = root.path().join("sessions");
    fs::create_dir_all(&session_dir).unwrap();
    let inbox_dir = base_dir
        .join("runtime")
        .join(TEAM_RUN_ID)
        .join("inboxes")
        .join("alice");
    Harness {
        config: TeamModeConfig::with_base_dir(base_dir.to_string_lossy().into_owned()),
        inbox_dir,
        session_dir,
        injected: Arc::new(Mutex::new(Vec::new())),
        events: Arc::new(Mutex::new(Vec::new())),
        _root: root,
    }
}

fn message(message_id: &str) -> Message {
    Message::safe_parse(&json!({
        "version": 1,
        "messageId": message_id,
        "from": "lead",
        "to": "alice",
        "kind": "message",
        "body": "hello",
        "timestamp": 1,
    }))
    .unwrap()
}

/// Writes the unread inbox file exactly where the lead's send would place it.
fn seed(harness: &Harness, value: &Message) {
    fs::create_dir_all(&harness.inbox_dir).unwrap();
    let path = harness.inbox_dir.join(format!("{}.json", value.message_id));
    fs::write(path, serde_json::to_string(value).unwrap()).unwrap();
}

fn poller_with(harness: &Harness, after_inject: Option<QaAfterInjectHold>) -> MemberSelfPoller {
    let injected = Arc::clone(&harness.injected);
    let events = Arc::clone(&harness.events);
    create_member_self_poller(MemberSelfPollerDeps {
        team_run_id: TEAM_RUN_ID.to_string(),
        member_name: "alice".to_string(),
        config: harness.config.clone(),
        session_dir: harness.session_dir.clone(),
        inject: Box::new(move |content| injected.lock().unwrap().push(content.to_string())),
        append_event: Some(Box::new(move |event| events.lock().unwrap().push(event))),
        after_inject,
    })
}

fn poller(harness: &Harness) -> MemberSelfPoller {
    poller_with(harness, None)
}

fn persist_envelope(harness: &Harness, value: &Message) {
    let entry = json!({
        "type": "message",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": build_peer_message_envelope(value) }],
        },
    });
    fs::write(
        harness.session_dir.join("20260712_session.jsonl"),
        format!("{}\n", serde_json::to_string(&entry).unwrap()),
    )
    .unwrap();
}

#[test]
fn given_after_inject_hold_when_delivery_reaches_crash_window_then_reservation_stays_uncommitted_until_released() {
    // given
    let harness = create_harness();
    let value = message("11111111-1111-4111-8111-111111111111");
    seed(&harness, &value);
    let observed: ObservedInHold = Arc::new(Mutex::new(None));
    let observed_in_hold = Arc::clone(&observed);
    let injected_in_hold = Arc::clone(&harness.injected);
    let inbox_dir = harness.inbox_dir.clone();
    let hold: QaAfterInjectHold = Box::new(move |held: &Message| {
        let delivering = inbox_dir
            .join(format!(".delivering-{}.json", held.message_id))
            .exists();
        let processed = inbox_dir
            .join("processed")
            .join(format!("{}.json", held.message_id))
            .exists();
        let injected = injected_in_hold.lock().unwrap().clone();
        *observed_in_hold.lock().unwrap() = Some((injected, delivering, processed));
        Ok(())
    });
    let self_poller = poller_with(&harness, Some(hold));

    // when
    self_poller.poll_once(None).unwrap();

    // then
    let (injected, delivering, processed) = observed.lock().unwrap().clone().expect("hold entered");
    assert_eq!(injected, vec![build_peer_message_envelope(&value)]);
    assert!(delivering);
    assert!(!processed);
}

#[test]
fn given_unread_message_when_envelope_reaches_session_jsonl_then_commit_happens_only_after_persistence() {
    // given
    let harness = create_harness();
    let value = message("22222222-2222-4222-8222-222222222222");
    seed(&harness, &value);
    let self_poller = poller(&harness);

    // when
    self_poller.poll_once(None).unwrap();

    // then
    assert_eq!(harness.injected(), vec![build_peer_message_envelope(&value)]);
    assert!(
        harness
            .inbox_dir
            .join(format!(".delivering-{}.json", value.message_id))
            .exists()
    );
    assert!(
        !harness
            .inbox_dir
            .join("processed")
            .join(format!("{}.json", value.message_id))
            .exists()
    );

    // when durable acknowledgement appears
    persist_envelope(&harness, &value);
    self_poller.check_pending_acks().unwrap();

    // then
    assert!(
        harness
            .inbox_dir
            .join("processed")
            .join(format!("{}.json", value.message_id))
            .exists()
    );
    assert_eq!(
        harness.events().last().cloned(),
        Some(TeamMessageEvent {
            event_type: "team_message_delivered".to_string(),
            payload: json!({
                "message_id": value.message_id,
                "from": "lead",
                "to": "alice",
                "kind": "message",
            }),
        })
    );
}

#[test]
fn given_inject_without_ack_when_ack_checks_repeat_then_reservation_is_held_and_never_double_injected() {
    // given
    let harness = create_harness();
    let value = message("33333333-3333-4333-8333-333333333333");
    seed(&harness, &value);
    let self_poller = poller(&harness);

    // when
    self_poller.poll_once(None).unwrap();
    self_poller.check_pending_acks().unwrap();
    self_poller.poll_once(None).unwrap();

    // then
    assert_eq!(harness.injected().len(), 1);
    assert!(
        harness
            .inbox_dir
            .join(format!(".delivering-{}.json", value.message_id))
            .exists()
    );
    assert!(!harness.inbox_dir.join(format!("{}.json", value.message_id)).exists());

    // when
    persist_envelope(&harness, &value);
    self_poller.check_pending_acks().unwrap();

    // then
    assert_eq!(harness.injected().len(), 1);
    assert!(
        harness
            .inbox_dir
            .join("processed")
            .join(format!("{}.json", value.message_id))
            .exists()
    );
}

#[test]
fn given_crash_after_envelope_persistence_when_new_poller_recovers_then_commits_without_reinjecting() {
    // given
    let harness = create_harness();
    let value = message("44444444-4444-4444-8444-444444444444");
    seed(&harness, &value);
    poller(&harness).poll_once(None).unwrap();
    persist_envelope(&harness, &value);

    // when
    poller(&harness).recover_reservations().unwrap();

    // then
    assert_eq!(harness.injected().len(), 1);
    assert!(
        harness
            .inbox_dir
            .join("processed")
            .join(format!("{}.json", value.message_id))
            .exists()
    );
    assert!(
        !harness
            .inbox_dir
            .join(format!(".delivering-{}.json", value.message_id))
            .exists()
    );
}

#[test]
fn given_consumed_ledger_stray_when_poller_sees_duplicate_unread_file_then_acks_without_injecting() {
    // given
    let harness = create_harness();
    let value = message("55555555-5555-4555-8555-555555555555");
    seed(&harness, &value);
    let reservation =
        reserve_message_for_delivery(TEAM_RUN_ID, "alice", &value.message_id, &harness.config).unwrap();
    let reservation = reservation.expect("reservation");
    commit_delivery_reservation(&reservation).unwrap();
    seed(&harness, &value);

    // when
    poller(&harness).poll_once(None).unwrap();

    // then
    assert_eq!(harness.injected(), Vec::<String>::new());
    assert!(!harness.inbox_dir.join(format!("{}.json", value.message_id)).exists());
    assert!(
        harness
            .inbox_dir
            .join("processed")
            .join(format!("{}.json", value.message_id))
            .exists()
    );
}
