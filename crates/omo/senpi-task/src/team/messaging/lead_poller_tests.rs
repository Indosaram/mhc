//! `team/messaging/lead-poller.test.ts`

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::TeamModeConfig;
use team_core::types::Message;

use crate::team::messaging::delivery_journal::{LeadDeliveryJournalOptions, create_lead_delivery_journal};
use crate::team::messaging::lead_poller::{TeamLeadPoller, create_lead_poller};
use crate::team::messaging::lead_poller_types::{LeadInjection, LeadInjectionSink, LeadPoller, LeadPollerDeps};
use crate::team::messaging::message::build_peer_message_envelope;

const TEAM_RUN_ID: &str = "11111111-1111-4111-8111-111111111111";

struct RecordingSink {
    injections: Arc<Mutex<Vec<LeadInjection>>>,
}

impl LeadInjectionSink for RecordingSink {
    fn enqueue(&self, injection: LeadInjection) {
        self.injections.lock().expect("injections lock").push(injection);
    }
}

struct Harness {
    _root: tempfile::TempDir,
    config: TeamModeConfig,
    inbox_dir: PathBuf,
    session_file: PathBuf,
    session_file_path: Arc<Mutex<Option<PathBuf>>>,
    injections: Arc<Mutex<Vec<LeadInjection>>>,
}

fn create_harness(session_available: bool) -> Harness {
    let root = tempfile::tempdir().expect("tempdir");
    let base_dir = root.path().join("teams");
    let session_dir = root.path().join("sessions");
    let session_file = session_dir.join("20260712_lead.jsonl");
    std::fs::create_dir_all(&session_dir).expect("create session dir");
    let config = TeamModeConfig::parse(&json!({ "base_dir": base_dir.to_string_lossy() })).expect("config parses");
    let inbox_dir = base_dir.join("runtime").join(TEAM_RUN_ID).join("inboxes").join("lead");
    Harness {
        _root: root,
        config,
        inbox_dir,
        session_file_path: Arc::new(Mutex::new(if session_available {
            Some(session_file.clone())
        } else {
            None
        })),
        session_file,
        injections: Arc::new(Mutex::new(Vec::new())),
    }
}

fn message(message_id: &str, body: &str) -> Message {
    Message::safe_parse(&json!({
        "version": 1,
        "messageId": message_id,
        "from": "alpha",
        "to": "lead",
        "kind": "message",
        "body": body,
        "timestamp": 1,
    }))
    .expect("valid message")
}

/// Seeds the lead inbox with an unread message, as a member send would.
fn seed(harness: &Harness, value: &Message) {
    std::fs::create_dir_all(&harness.inbox_dir).expect("create inbox dir");
    let path = harness.inbox_dir.join(format!("{}.json", value.message_id));
    let raw = serde_json::to_string(value).expect("serialize message");
    std::fs::write(path, raw).expect("write inbox message");
}

fn poller(harness: &Harness) -> TeamLeadPoller {
    let session_file_path = Arc::clone(&harness.session_file_path);
    create_lead_poller(LeadPollerDeps {
        team_run_id: TEAM_RUN_ID.to_string(),
        config: harness.config.clone(),
        coordinator: Arc::new(RecordingSink {
            injections: Arc::clone(&harness.injections),
        }),
        delivery_journal: Some(Arc::new(create_lead_delivery_journal(LeadDeliveryJournalOptions::default()))),
        append_event: Some(Box::new(|_, _| {})),
        event_task_id: Box::new(|value: &Message| {
            if value.from == "alpha" {
                Some("st_00000001".to_string())
            } else {
                None
            }
        }),
        lead_session_file: Some(Box::new(move || {
            session_file_path.lock().expect("session path lock").clone()
        })),
    })
}

fn persist_envelope(harness: &Harness, value: &Message) {
    let entry = json!({
        "type": "message",
        "message": { "role": "user", "content": build_peer_message_envelope(value) },
    });
    std::fs::write(&harness.session_file, format!("{entry}\n")).expect("write session file");
}

fn flush_latest(harness: &Harness) {
    let callback = {
        let mut injections = harness.injections.lock().expect("injections lock");
        let injection = injections.last_mut().expect("expected a pending lead injection");
        injection.on_flushed.take()
    };
    if let Some(callback) = callback {
        callback();
    }
}

fn processed_path(harness: &Harness, value: &Message) -> PathBuf {
    harness.inbox_dir.join("processed").join(format!("{}.json", value.message_id))
}

fn injection_count(harness: &Harness) -> usize {
    harness.injections.lock().expect("injections lock").len()
}

#[test]
fn rejected_wake_restores_unread_reservation_and_allows_redelivery() {
    let harness = create_harness(false);
    let value = message("66666666-6666-4666-8666-666666666666", "ready");
    seed(&harness, &value);
    let lead_poller = poller(&harness);
    lead_poller.poll_once(None).expect("poll");
    let callback = harness.injections.lock().expect("injections")[0].on_delivery_failed.take().expect("failure callback");
    callback("wake rejected");
    assert!(harness.inbox_dir.join(format!("{}.json", value.message_id)).exists());
    assert!(!harness.inbox_dir.join(format!(".delivering-{}.json", value.message_id)).exists());
    assert!(!processed_path(&harness, &value).exists());
    lead_poller.poll_once(None).expect("redelivery");
    assert_eq!(injection_count(&harness), 2);
}

fn exists(path: &Path) -> bool {
    path.exists()
}

#[test]
fn given_member_mail_when_the_lead_polls_then_it_injects_the_peer_envelope_without_a_blocking_tool() {
    // given
    let harness = create_harness(true);
    let value = message("22222222-2222-4222-8222-222222222222", "member report");
    seed(&harness, &value);
    let lead_poller = poller(&harness);

    // when
    lead_poller.poll_once(None).expect("poll");

    // then
    let contents: Vec<String> = harness
        .injections
        .lock()
        .expect("injections lock")
        .iter()
        .map(|entry| entry.content.clone())
        .collect();
    assert_eq!(contents, vec![build_peer_message_envelope(&value)]);
    assert_eq!(exists(&processed_path(&harness, &value)), false);

    // when the injection reaches the durable lead transcript
    persist_envelope(&harness, &value);
    flush_latest(&harness);
    lead_poller.poll_once(None).expect("poll");

    // then mailbox commit follows durable delivery
    assert_eq!(exists(&processed_path(&harness, &value)), true);
}

#[test]
fn given_a_crash_before_the_lead_injection_flushes_when_a_fresh_poller_resumes_then_the_durable_mailbox_redelivers() {
    // given
    let harness = create_harness(true);
    let value = message("33333333-3333-4333-8333-333333333333", "ready");
    seed(&harness, &value);
    poller(&harness).poll_once(None).expect("poll");

    // when
    poller(&harness).poll_once(None).expect("poll");

    // then
    assert_eq!(injection_count(&harness), 2);
    assert_eq!(
        exists(&harness.inbox_dir.join(format!(".delivering-{}.json", value.message_id))),
        true
    );
    assert_eq!(exists(&processed_path(&harness, &value)), false);
}

#[test]
fn given_a_crash_after_lead_transcript_persistence_when_a_fresh_poller_resumes_then_it_commits_without_reinjecting() {
    // given
    let harness = create_harness(true);
    let value = message("44444444-4444-4444-8444-444444444444", "ready");
    seed(&harness, &value);
    poller(&harness).poll_once(None).expect("poll");
    persist_envelope(&harness, &value);

    // when
    poller(&harness).poll_once(None).expect("poll");

    // then
    assert_eq!(injection_count(&harness), 1);
    assert_eq!(exists(&processed_path(&harness, &value)), true);
}

#[test]
fn given_no_captured_lead_transcript_when_mail_flushes_then_the_reservation_holds_until_persistence_becomes_observable() {
    // given
    let harness = create_harness(false);
    let value = message("55555555-5555-4555-8555-555555555555", "ready");
    seed(&harness, &value);
    let lead_poller = poller(&harness);
    lead_poller.poll_once(None).expect("poll");
    flush_latest(&harness);

    // when
    lead_poller.poll_once(None).expect("poll");

    // then
    assert_eq!(exists(&processed_path(&harness, &value)), false);

    // when a transcript is captured and records the injected envelope
    *harness.session_file_path.lock().expect("session path lock") = Some(harness.session_file.clone());
    persist_envelope(&harness, &value);
    lead_poller.poll_once(None).expect("poll");

    // then
    assert_eq!(exists(&processed_path(&harness, &value)), true);
}
