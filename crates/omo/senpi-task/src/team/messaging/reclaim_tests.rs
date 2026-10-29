//! `team/messaging/reclaim.test.ts`

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use team_core::team_mailbox::reserve_message_for_delivery;
use team_core::types::Message;

use crate::store::StateDirConfig;
use crate::team::messaging::message::{BuildTeamMessageOptions, build_team_message, random_uuid};
use crate::team::messaging::messaging_fakes::state_dir_config;
use crate::team::messaging::reclaim::{ReclaimResult, reclaim_stale_team_reservations};
use crate::team::messaging::types::SendTeamMessageInput;
use crate::team::runtime_config::{TeamCoreConfig, to_team_core_config};
use crate::team::runtime_fakes::{TeamBoundsOverrides, team_bounds};
use crate::team::storage::{resolve_team_member_inbox_dir, team_storage_base_dir};

const TEAM_RUN_ID: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const STALE_TTL_MS: i64 = 30_000;

struct Setup {
    // Held so the temp project dir lives for the whole test (per-test cleanup, like afterEach).
    _root: TempDir,
    state_dir: StateDirConfig,
    config: TeamCoreConfig,
}

fn setup() -> Setup {
    let root = tempfile::Builder::new()
        .prefix("senpi-team-messaging-")
        .tempdir()
        .expect("create temp project dir");
    let state_dir = state_dir_config(root.path());
    let base_dir = team_storage_base_dir(&state_dir);
    let config = to_team_core_config(
        &team_bounds(TeamBoundsOverrides::default()),
        &base_dir.to_string_lossy(),
    )
    .expect("team core config");
    Setup {
        _root: root,
        state_dir,
        config,
    }
}

fn inbox_dir(state_dir: &StateDirConfig, recipient: &str) -> PathBuf {
    resolve_team_member_inbox_dir(state_dir, TEAM_RUN_ID, recipient).expect("inbox dir")
}

fn reserved_path(state_dir: &StateDirConfig, recipient: &str, message_id: &str) -> PathBuf {
    inbox_dir(state_dir, recipient).join(format!(".delivering-{message_id}.json"))
}

fn set_mtime(path: &Path, time: SystemTime) {
    let file = OpenOptions::new().write(true).open(path).expect("open reserved file");
    file.set_modified(time).expect("set mtime");
}

/// Delivers one unread message into the recipient inbox, then reserves it for delivery.
/// Returns the message id.
fn reserve_one(setup: &Setup, recipient: &str, age: bool) -> String {
    let message_id = random_uuid();
    let id_for_builder = message_id.clone();
    let new_message_id = move || id_for_builder.clone();
    let message: Message = build_team_message(
        &SendTeamMessageInput {
            from: "alpha".to_string(),
            to: recipient.to_string(),
            body: "b".to_string(),
            summary: None,
        },
        &BuildTeamMessageOptions {
            now: None,
            new_message_id: Some(&new_message_id),
        },
    )
    .expect("build message");

    let inbox = inbox_dir(&setup.state_dir, recipient);
    fs::create_dir_all(&inbox).expect("create inbox dir");
    fs::write(
        inbox.join(format!("{message_id}.json")),
        serde_json::to_vec(&message).expect("serialize message"),
    )
    .expect("write unread message");

    let _reservation = reserve_message_for_delivery(TEAM_RUN_ID, recipient, &message_id, &setup.config)
        .expect("reserve message");

    if age {
        let past = SystemTime::now() - Duration::from_millis((STALE_TTL_MS * 4) as u64);
        set_mtime(&reserved_path(&setup.state_dir, recipient, &message_id), past);
    }
    message_id
}

#[test]
fn given_stale_reservation_older_than_ttl_when_reclaimed_then_restored_to_unread() {
    // given
    let setup = setup();
    let message_id = reserve_one(&setup, "beta", true);
    let inbox = inbox_dir(&setup.state_dir, "beta");

    // when
    let reclaimed =
        reclaim_stale_team_reservations(TEAM_RUN_ID, &["beta"], &setup.config, STALE_TTL_MS).expect("reclaim");

    // then
    let mut expected = ReclaimResult::new();
    expected.insert("beta".to_string(), vec![message_id.clone()]);
    assert_eq!(reclaimed, expected);
    assert!(inbox.join(format!("{message_id}.json")).exists());
    assert!(!reserved_path(&setup.state_dir, "beta", &message_id).exists());
}

#[test]
fn given_fresh_reservation_within_ttl_when_reclaimed_then_nothing_restored() {
    // given
    let setup = setup();
    reserve_one(&setup, "beta", false);

    // when
    let reclaimed =
        reclaim_stale_team_reservations(TEAM_RUN_ID, &["beta"], &setup.config, STALE_TTL_MS).expect("reclaim");

    // then
    let mut expected = ReclaimResult::new();
    expected.insert("beta".to_string(), Vec::new());
    assert_eq!(reclaimed, expected);
}

#[test]
fn given_multiple_members_when_reclaimed_then_each_members_stale_reservations_reported_per_name() {
    // given
    let setup = setup();
    let beta = reserve_one(&setup, "beta", true);
    let gamma = reserve_one(&setup, "gamma", true);

    // when
    let reclaimed = reclaim_stale_team_reservations(TEAM_RUN_ID, &["beta", "gamma"], &setup.config, STALE_TTL_MS)
        .expect("reclaim");

    // then
    let mut expected = ReclaimResult::new();
    expected.insert("beta".to_string(), vec![beta]);
    expected.insert("gamma".to_string(), vec![gamma]);
    assert_eq!(reclaimed, expected);
}
