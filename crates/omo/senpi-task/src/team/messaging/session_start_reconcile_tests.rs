//! `team/messaging/session-start-reconcile.test.ts`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use pretty_assertions::assert_eq;
use serde_json::json;
use team_core::types::Message;

use crate::store::StateDirConfig;
use crate::team::messaging::message::{BuildTeamMessageOptions, build_team_message};
use crate::team::messaging::messaging_fakes::state_dir_config;
use crate::team::messaging::session_start_reconcile::{
    ReconcileTeamMailboxDeps, reconcile_team_mailbox_on_session_start,
};
use crate::team::messaging::types::SendTeamMessageInput;
use crate::team::normalize::{TEAM_LEAD_SENTINEL, normalize_senpi_team_spec};
use crate::team::registry::TeamSpecSource;
use crate::team::runtime::create_team;
use crate::team::runtime_config::{TeamCoreConfig, to_team_core_config};
use crate::team::runtime_fakes::{FakeTeamManager, FakeTeamManagerOptions, TeamBoundsOverrides, team_bounds};
use crate::team::runtime_types::CreateTeamDeps;
use crate::team::storage::{resolve_team_member_inbox_dir, team_storage_base_dir};

const STALE_TTL_MS: i64 = 30_000;

struct ActiveTeam {
    _project: tempfile::TempDir,
    project_dir: PathBuf,
    config: TeamCoreConfig,
    team_run_id: String,
    member: String,
}

impl ActiveTeam {
    fn state_dir(&self) -> StateDirConfig {
        state_dir_config(&self.project_dir)
    }
}

fn core_config(state_dir: &StateDirConfig) -> TeamCoreConfig {
    to_team_core_config(
        &team_bounds(TeamBoundsOverrides::default()),
        &team_storage_base_dir(state_dir).to_string_lossy(),
    )
    .expect("team core config")
}

fn active_team_with_member(member: &str) -> ActiveTeam {
    let project = tempfile::tempdir().expect("tempdir");
    let project_dir = project.path().to_path_buf();
    let config = core_config(&state_dir_config(&project_dir));
    let spec = normalize_senpi_team_spec(
        &json!({ "members": [{ "name": member, "kind": "category", "category": "quick", "prompt": "p" }] }),
        "squad",
        None,
    )
    .expect("normalize spec");
    let created = create_team(
        &spec,
        TeamSpecSource::Project,
        &CreateTeamDeps {
            manager: Arc::new(FakeTeamManager::new(FakeTeamManagerOptions::default())),
            state_dir: state_dir_config(&project_dir),
            team_bounds: team_bounds(TeamBoundsOverrides::default()),
            lead_session_id: "lead-session".to_string(),
            spawn_depth: 1,
            now: None,
            member_extension: None,
            write_member_map: None,
        },
    )
    .expect("create team");
    ActiveTeam {
        _project: project,
        project_dir,
        config,
        team_run_id: created.runtime_state.team_run_id,
        member: member.to_string(),
    }
}

fn build_message(from: &str, to: &str, body: &str) -> Message {
    build_team_message(
        &SendTeamMessageInput {
            from: from.to_string(),
            to: to.to_string(),
            body: body.to_string(),
            summary: None,
        },
        &BuildTeamMessageOptions::default(),
    )
    .expect("build message")
}

// Delivers the message to the recipient's unread inbox, then moves it into the
// `.delivering-<id>.json` reservation slot (what reserveMessageForDelivery does).
fn deliver_and_reserve(inbox_dir: &Path, message: &Message) -> PathBuf {
    fs::create_dir_all(inbox_dir).expect("create inbox dir");
    let unread = inbox_dir.join(format!("{}.json", message.message_id));
    fs::write(&unread, serde_json::to_vec(message).expect("serialize message")).expect("write unread");
    let reserved = inbox_dir.join(format!(".delivering-{}.json", message.message_id));
    fs::rename(&unread, &reserved).expect("reserve message");
    reserved
}

fn age(path: &Path) {
    let past = SystemTime::now() - Duration::from_millis((STALE_TTL_MS * 4) as u64);
    let file = fs::File::options().write(true).open(path).expect("open reserved");
    file.set_modified(past).expect("set mtime");
}

fn reserve_and_age(team: &ActiveTeam, aged: bool) -> (Message, PathBuf) {
    let message = build_message("alpha", &team.member, "b");
    let inbox_dir =
        resolve_team_member_inbox_dir(&team.state_dir(), &team.team_run_id, &team.member).expect("inbox dir");
    let reserved = deliver_and_reserve(&inbox_dir, &message);
    if aged {
        age(&reserved);
    }
    (message, reserved)
}

fn reserve_lead_and_age(team: &ActiveTeam) -> (Message, PathBuf, PathBuf) {
    let message = build_message(&team.member, TEAM_LEAD_SENTINEL, "lead note");
    let inbox_dir = resolve_team_member_inbox_dir(&team.state_dir(), &team.team_run_id, TEAM_LEAD_SENTINEL)
        .expect("lead inbox dir");
    let reserved = deliver_and_reserve(&inbox_dir, &message);
    age(&reserved);
    (message, inbox_dir, reserved)
}

#[test]
fn given_an_active_team_with_a_stale_reservation_when_reconciled_then_the_reservation_is_restored_to_unread() {
    // given
    let team = active_team_with_member("beta");
    let inbox_dir =
        resolve_team_member_inbox_dir(&team.state_dir(), &team.team_run_id, &team.member).expect("inbox dir");
    let (message, reserved) = reserve_and_age(&team, true);

    // when
    reconcile_team_mailbox_on_session_start(&ReconcileTeamMailboxDeps {
        state_dir: team.state_dir(),
        config: team.config.clone(),
        stale_ttl_ms: Some(STALE_TTL_MS),
        current_lead_session_id: None,
    });

    // then
    assert_eq!(inbox_dir.join(format!("{}.json", message.message_id)).exists(), true);
    assert_eq!(reserved.exists(), false);
}

#[test]
fn given_a_fresh_reservation_within_the_ttl_when_reconciled_then_it_is_left_reserved() {
    // given
    let team = active_team_with_member("beta");
    let (_message, reserved) = reserve_and_age(&team, false);

    // when
    reconcile_team_mailbox_on_session_start(&ReconcileTeamMailboxDeps {
        state_dir: team.state_dir(),
        config: team.config.clone(),
        stale_ttl_ms: Some(STALE_TTL_MS),
        current_lead_session_id: None,
    });

    // then
    assert_eq!(reserved.exists(), true);
}

#[test]
fn given_an_owned_team_with_a_stale_lead_reservation_when_reconciled_then_the_lead_inbox_is_restored() {
    // given
    let team = active_team_with_member("beta");
    let (message, inbox_dir, reserved) = reserve_lead_and_age(&team);

    // when
    reconcile_team_mailbox_on_session_start(&ReconcileTeamMailboxDeps {
        state_dir: team.state_dir(),
        config: team.config.clone(),
        stale_ttl_ms: Some(STALE_TTL_MS),
        current_lead_session_id: Some("lead-session".to_string()),
    });

    // then
    assert_eq!(inbox_dir.join(format!("{}.json", message.message_id)).exists(), true);
    assert_eq!(reserved.exists(), false);
}

#[test]
fn given_a_foreign_team_with_a_stale_lead_reservation_when_reconciled_then_its_lead_inbox_is_untouched() {
    // given
    let team = active_team_with_member("beta");
    let (_message, _inbox_dir, reserved) = reserve_lead_and_age(&team);

    // when
    reconcile_team_mailbox_on_session_start(&ReconcileTeamMailboxDeps {
        state_dir: team.state_dir(),
        config: team.config.clone(),
        stale_ttl_ms: Some(STALE_TTL_MS),
        current_lead_session_id: Some("another-session".to_string()),
    });

    // then
    assert_eq!(reserved.exists(), true);
}

#[test]
fn given_no_active_teams_when_reconciled_then_it_is_a_no_op_that_does_not_throw() {
    // given
    let project = tempfile::tempdir().expect("tempdir");
    let state_dir = state_dir_config(project.path());
    let config = core_config(&state_dir);

    // when / then
    reconcile_team_mailbox_on_session_start(&ReconcileTeamMailboxDeps {
        state_dir,
        config,
        stale_ttl_ms: Some(STALE_TTL_MS),
        current_lead_session_id: None,
    });
}
