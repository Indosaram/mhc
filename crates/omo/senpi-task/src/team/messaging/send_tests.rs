//! `team/messaging/send.test.ts`

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::store::StateDirConfig;
use crate::team::member_map::{MemberTaskMap, write_member_task_map};
use crate::team::messaging::lead_poller_types::{AppendEventFn, TeamTaskEvent};
use crate::team::messaging::messaging_fakes::{state_dir_config, temp_project_dir};
use crate::team::messaging::send::send_team_message;
use crate::team::messaging::types::{MessagingEngineDeps, SendTeamMessageInput, SendTeamMessageResult};
use crate::team::runtime_config::{TeamCoreConfig, to_team_core_config};
use crate::team::runtime_fakes::{TeamBoundsOverrides, team_bounds};
use crate::team::storage::{
    ensure_team_runtime_dirs, resolve_team_member_inbox_dir, resolve_team_runtime_dirs, team_storage_base_dir,
};

const TEAM_RUN_ID: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

type Appended = Arc<Mutex<Vec<(String, TeamTaskEvent)>>>;

fn member_map(entries: &[(&str, &str)]) -> MemberTaskMap {
    entries
        .iter()
        .map(|(name, task_id)| ((*name).to_string(), (*task_id).to_string()))
        .collect()
}

fn setup(map: &MemberTaskMap) -> (StateDirConfig, TeamCoreConfig) {
    let state_dir = state_dir_config(&temp_project_dir());
    let base_dir = team_storage_base_dir(&state_dir);
    let config = to_team_core_config(&team_bounds(TeamBoundsOverrides::default()), &base_dir.to_string_lossy())
        .expect("team core config");
    let names: Vec<String> = map.keys().cloned().collect();
    ensure_team_runtime_dirs(&state_dir, TEAM_RUN_ID, &names).expect("ensure runtime dirs");
    let runtime_dir = resolve_team_runtime_dirs(&state_dir, TEAM_RUN_ID)
        .expect("runtime dirs")
        .runtime_dir;
    write_member_task_map(&runtime_dir, map).expect("write member map");
    (state_dir, config)
}

fn deps(
    state_dir: &StateDirConfig,
    config: TeamCoreConfig,
    map: &MemberTaskMap,
    message_id: Option<&'static str>,
    append_event: Option<AppendEventFn>,
) -> MessagingEngineDeps {
    MessagingEngineDeps {
        team_run_id: TEAM_RUN_ID.to_string(),
        state_dir: state_dir.clone(),
        config,
        active_members: map.keys().cloned().collect(),
        append_event,
        now: None,
        new_message_id: message_id.map(|id| Box::new(move || id.to_string()) as Box<dyn Fn() -> String + Send + Sync>),
    }
}

fn recorder() -> (Appended, AppendEventFn) {
    let appended: Appended = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&appended);
    let append: AppendEventFn = Box::new(move |task_id: &str, event: TeamTaskEvent| {
        sink.lock().expect("lock appended").push((task_id.to_string(), event));
    });
    (appended, append)
}

fn input(from: &str, to: &str, body: &str) -> SendTeamMessageInput {
    SendTeamMessageInput {
        from: from.to_string(),
        to: to.to_string(),
        body: body.to_string(),
        summary: None,
    }
}

fn inbox_dir(state_dir: &StateDirConfig, recipient: &str) -> PathBuf {
    resolve_team_member_inbox_dir(state_dir, TEAM_RUN_ID, recipient).expect("inbox dir")
}

fn unread_path(state_dir: &StateDirConfig, recipient: &str, message_id: &str) -> PathBuf {
    inbox_dir(state_dir, recipient).join(format!("{message_id}.json"))
}

fn processed_path(state_dir: &StateDirConfig, recipient: &str, message_id: &str) -> PathBuf {
    inbox_dir(state_dir, recipient)
        .join("processed")
        .join(format!("{message_id}.json"))
}

fn unread_files(state_dir: &StateDirConfig, recipient: &str) -> Vec<String> {
    let mut files: Vec<String> = std::fs::read_dir(inbox_dir(state_dir, recipient))
        .expect("read inbox")
        .map(|entry| entry.expect("dir entry").file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json") && !name.starts_with('.'))
        .collect();
    files.sort();
    files
}

#[test]
fn given_an_unknown_recipient_when_the_lead_sends_then_the_error_names_the_team_members_and_valid_forms() {
    // given
    let map = member_map(&[("alpha", "st_alpha"), ("beta", "st_beta")]);
    let (state_dir, config) = setup(&map);

    // when
    let error = send_team_message(&input("lead", "ghost", "hello"), &deps(&state_dir, config, &map, None, None))
        .expect_err("unknown recipient must fail");

    // then
    let pattern = regex::Regex::new(r"ghost[\s\S]*alpha[\s\S]*beta").expect("regex");
    assert!(pattern.is_match(&error.to_string()), "unexpected message: {error}");
}

#[test]
fn given_an_unknown_recipient_when_the_lead_sends_then_the_stable_invalid_recipient_error_name_survives() {
    // given
    let map = member_map(&[("alpha", "st_alpha")]);
    let (state_dir, config) = setup(&map);

    // when
    let error = send_team_message(&input("lead", "ghost", "hello"), &deps(&state_dir, config, &map, None, None))
        .expect_err("unknown recipient must fail");

    // then
    assert_eq!(error.name(), "InvalidRecipientError");
}

#[test]
fn given_a_member_recipient_when_a_member_sends_then_one_unread_file_is_written_and_the_send_returns_enqueued() {
    // given
    let map = member_map(&[("alpha", "st_a"), ("beta", "st_b")]);
    let (state_dir, config) = setup(&map);
    let message_id = "11111111-1111-4111-8111-111111111111";

    // when
    let result = send_team_message(
        &input("alpha", "beta", "ping"),
        &deps(&state_dir, config, &map, Some(message_id), None),
    )
    .expect("send");

    // then
    assert_eq!(
        result,
        SendTeamMessageResult::ToMembers {
            message_id: message_id.to_string(),
            recipients: vec!["beta".to_string()],
        }
    );
    assert_eq!(result.kind(), "to_members");
    assert_eq!(unread_files(&state_dir, "beta"), vec![format!("{message_id}.json")]);
    assert!(!processed_path(&state_dir, "beta", message_id).exists());
}

#[test]
fn given_the_lead_sentinel_when_a_member_sends_then_the_lead_inbox_receives_an_unread_file() {
    // given
    let map = member_map(&[("alpha", "st_a")]);
    let (state_dir, config) = setup(&map);
    let message_id = "22222222-2222-4222-8222-222222222222";

    // when
    let result = send_team_message(
        &input("alpha", "lead", "need a call"),
        &deps(&state_dir, config, &map, Some(message_id), None),
    )
    .expect("send");

    // then
    assert_eq!(
        result,
        SendTeamMessageResult::ToLead {
            message_id: message_id.to_string(),
        }
    );
    assert_eq!(result.kind(), "to_lead");
    assert_eq!(unread_files(&state_dir, "lead"), vec![format!("{message_id}.json")]);
    assert!(!processed_path(&state_dir, "lead", message_id).exists());
}

#[test]
fn given_three_active_members_when_the_lead_broadcasts_then_every_recipient_inbox_stays_unread() {
    // given
    let map = member_map(&[("alpha", "st_a"), ("beta", "st_b"), ("gamma", "st_g")]);
    let (state_dir, config) = setup(&map);
    let message_id = "33333333-3333-4333-8333-333333333333";
    let (appended, append) = recorder();

    // when
    let result = send_team_message(
        &input("lead", "*", "all-hands"),
        &deps(&state_dir, config, &map, Some(message_id), Some(append)),
    )
    .expect("send");

    // then
    assert_eq!(
        result,
        SendTeamMessageResult::ToMembers {
            message_id: message_id.to_string(),
            recipients: vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        }
    );
    for member in ["alpha", "beta", "gamma"] {
        assert!(unread_path(&state_dir, member, message_id).exists());
        assert!(!processed_path(&state_dir, member, message_id).exists());
    }
    let task_ids: Vec<String> = appended
        .lock()
        .expect("lock appended")
        .iter()
        .map(|(task_id, _)| task_id.clone())
        .collect();
    assert_eq!(task_ids, vec!["st_a".to_string(), "st_b".to_string(), "st_g".to_string()]);
}

#[test]
fn given_a_member_task_mapping_when_the_member_sends_then_team_message_sent_is_anchored_to_the_sender_record() {
    // given
    let map = member_map(&[("alpha", "st_a"), ("beta", "st_b")]);
    let (state_dir, config) = setup(&map);
    let message_id = "44444444-4444-4444-8444-444444444444";
    let (appended, append) = recorder();

    // when
    send_team_message(
        &input("alpha", "beta", "status"),
        &deps(&state_dir, config, &map, Some(message_id), Some(append)),
    )
    .expect("send");

    // then
    let appended = appended.lock().expect("lock appended");
    assert_eq!(appended.len(), 1);
    let (task_id, event) = &appended[0];
    assert_eq!(task_id, "st_a");
    assert_eq!(event.event_type, "team_message_sent");
    assert_eq!(
        event.payload,
        json!({ "message_id": message_id, "from": "alpha", "to": "beta", "kind": "message" })
    );
}

#[test]
fn given_no_append_event_dependency_when_the_lead_sends_then_enqueue_still_succeeds() {
    // given
    let map = member_map(&[("beta", "st_b")]);
    let (state_dir, config) = setup(&map);
    let message_id = "55555555-5555-4555-8555-555555555555";

    // when
    let result = send_team_message(
        &input("lead", "beta", "continue"),
        &deps(&state_dir, config, &map, Some(message_id), None),
    )
    .expect("send");

    // then
    assert_eq!(
        result,
        SendTeamMessageResult::ToMembers {
            message_id: message_id.to_string(),
            recipients: vec!["beta".to_string()],
        }
    );
}

#[test]
fn given_a_full_recipient_inbox_when_a_member_sends_then_recipient_backpressure_error_still_surfaces() {
    // given
    let map = member_map(&[("alpha", "st_a"), ("beta", "st_b")]);
    let (state_dir, mut config) = setup(&map);
    config.recipient_unread_max_bytes = 1;

    // when
    let attempt = send_team_message(
        &input("alpha", "beta", "x"),
        &deps(
            &state_dir,
            config,
            &map,
            Some("66666666-6666-4666-8666-666666666666"),
            None,
        ),
    );

    // then
    let error = attempt.expect_err("backpressure must fail");
    assert_eq!(error.name(), "RecipientBackpressureError");
}
