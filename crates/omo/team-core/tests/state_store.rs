//! Translated from src/team-state-store/{locks,store}.test.ts.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use team_core::TeamCoreError;
use team_core::TeamModeConfig;
use team_core::session_client::{
    SessionClientError, SessionLookupResponse, TeamSessionClient, TeamSessionContext,
};
use team_core::team_registry::paths::{get_inbox_dir, resolve_base_dir};
use team_core::team_state_store::locks::{
    AtomicWriteDeps, LockOpenErrorDeps, LockReleaseDeps, assert_retryable_lock_open_error,
    assert_retryable_lock_open_error_with, atomic_write_with, detect_stale_lock, reap_stale_lock,
    reap_stale_lock_with, with_lock,
};
use team_core::team_state_store::{ResumeReport, resume_all_teams};
use team_core::team_state_store::{
    STALE_DELETING_TTL_MS, create_runtime_state, list_active_teams, load_runtime_state,
    save_runtime_state, transition_runtime_state,
};
use team_core::types::{
    ActiveTeamSummary, AgentType, Member, MemberStatus, RuntimeBounds, RuntimeState, RuntimeStatus,
    SpecSource, TeamSpec,
};
use tempfile::TempDir;

fn now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("ms")
}

fn temp(prefix: &str) -> TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("tempdir")
}

fn eperm() -> io::Error {
    io::Error::from_raw_os_error(libc::EPERM)
}

// --- locks.test.ts ---------------------------------------------------------------------------

#[test]
fn with_lock_serializes_concurrent_work() {
    let root = temp("locks-serialize-");
    let lock_path = Arc::new(root.path().join("lock"));
    let probe_path = Arc::new(root.path().join("probe.txt"));
    fs::write(&*probe_path, "ready").expect("probe");
    let active: Arc<Mutex<HashSet<&'static str>>> = Arc::default();
    let overlap: Arc<Mutex<Vec<&'static str>>> = Arc::default();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    let first = {
        let (lock_path, probe_path, active, overlap) = (
            Arc::clone(&lock_path),
            Arc::clone(&probe_path),
            Arc::clone(&active),
            Arc::clone(&overlap),
        );
        thread::spawn(move || {
            with_lock(
                &lock_path,
                || {
                    active.lock().expect("active").insert("first");
                    fs::write(&*probe_path, "first-start")?;
                    acquired_tx.send(()).expect("signal acquired");
                    release_rx.recv().expect("await release");
                    if active.lock().expect("active").contains("second") {
                        overlap.lock().expect("overlap").push("first");
                    }
                    active.lock().expect("active").remove("first");
                    Ok("first".to_owned())
                },
                None,
            )
        })
    };
    acquired_rx.recv().expect("first acquired");
    let second = {
        let (lock_path, probe_path, active, overlap) = (
            Arc::clone(&lock_path),
            Arc::clone(&probe_path),
            Arc::clone(&active),
            Arc::clone(&overlap),
        );
        thread::spawn(move || {
            with_lock(
                &lock_path,
                || {
                    active.lock().expect("active").insert("second");
                    if active.lock().expect("active").contains("first") {
                        overlap.lock().expect("overlap").push("second");
                    }
                    let probe = fs::read_to_string(&*probe_path)?;
                    active.lock().expect("active").remove("second");
                    Ok(probe)
                },
                None,
            )
        })
    };
    release_tx.send(()).expect("release first");
    let results = [
        first.join().expect("join").expect("first"),
        second.join().expect("join").expect("second"),
    ];
    assert_eq!(results[0], "first");
    assert_eq!(results.len(), 2);
    assert!(overlap.lock().expect("overlap").is_empty());
}

#[test]
fn atomic_write_leaves_no_partial_file_when_rename_fails() {
    let root = temp("locks-atomic-");
    let target = root.path().join("target.txt");
    fs::write(&target, "old content").expect("seed");
    let rename_calls = RefCell::new(Vec::new());
    let deps = AtomicWriteDeps {
        rename: Some(Box::new(|from: &Path, to: &Path| {
            rename_calls
                .borrow_mut()
                .push(format!("{}->{}", from.display(), to.display()));
            Err(io::Error::other("rename failed"))
        })),
        ..AtomicWriteDeps::default()
    };

    let error = atomic_write_with(&target, "new content", &deps).expect_err("rename fails");

    assert!(error.to_string().contains("rename failed"));
    assert_eq!(fs::read_to_string(&target).expect("read"), "old content");
    assert_eq!(rename_calls.borrow().len(), 1);
    let leftovers = fs::read_dir(root.path())
        .expect("read_dir")
        .flatten()
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("target.txt.tmp.")
        });
    assert!(!leftovers);
}

#[test]
fn lock_open_treats_eperm_as_contention_when_lock_path_exists() {
    let root = temp("locks-eperm-existing-");
    let lock_path = root.path().join("lock");
    fs::write(&lock_path, "owner\n123\n456\n").expect("lock");
    assert_retryable_lock_open_error(&lock_path, eperm()).expect("contention");
}

#[test]
fn lock_open_treats_eperm_access_probes_as_possible_contention() {
    let root = temp("locks-eperm-access-");
    let lock_path = root.path().join("lock");
    let access_calls = RefCell::new(Vec::<PathBuf>::new());
    let deps = LockOpenErrorDeps {
        access: Some(Box::new(|path: &Path| {
            access_calls.borrow_mut().push(path.to_path_buf());
            Err(eperm())
        })),
        platform: None,
    };
    assert_retryable_lock_open_error_with(&lock_path, eperm(), &deps).expect("contention");
    assert_eq!(*access_calls.borrow(), vec![lock_path]);
}

#[test]
fn lock_open_treats_windows_eperm_with_existing_parent_as_possible_contention() {
    let root = temp("locks-eperm-win-parent-");
    let lock_path = root.path().join("lock");
    let access_calls = RefCell::new(Vec::<PathBuf>::new());
    let deps = LockOpenErrorDeps {
        access: Some(Box::new(|path: &Path| {
            access_calls.borrow_mut().push(path.to_path_buf());
            if path == lock_path {
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
            Ok(())
        })),
        platform: Some("win32"),
    };
    assert_retryable_lock_open_error_with(&lock_path, eperm(), &deps).expect("contention");
    assert_eq!(
        *access_calls.borrow(),
        vec![lock_path.clone(), root.path().to_path_buf()]
    );
}

#[test]
fn lock_open_rethrows_eperm_when_lock_path_does_not_exist() {
    let root = temp("locks-eperm-missing-");
    let deps = LockOpenErrorDeps {
        access: None,
        platform: Some("linux"),
    };
    let error = assert_retryable_lock_open_error_with(&root.path().join("lock"), eperm(), &deps)
        .expect_err("rethrow");
    assert_eq!(error.raw_os_error(), Some(libc::EPERM));
}

#[test]
fn lock_open_rethrows_windows_eperm_when_lock_parent_does_not_exist() {
    let root = temp("locks-eperm-win-missing-");
    let deps = LockOpenErrorDeps {
        access: None,
        platform: Some("win32"),
    };
    let lock_path = root.path().join("missing").join("lock");
    let error =
        assert_retryable_lock_open_error_with(&lock_path, eperm(), &deps).expect_err("rethrow");
    assert_eq!(error.raw_os_error(), Some(libc::EPERM));
}

#[test]
fn lock_release_retries_transient_eperm_before_removing_the_lock_file() {
    let root = temp("locks-release-eperm-");
    let lock_path = root.path().join("lock");
    fs::write(&lock_path, "owner\n123\n456\n").expect("lock");
    let delay_calls = RefCell::new(Vec::new());
    let unlink_calls = RefCell::new(0);
    let deps = LockReleaseDeps {
        delay: Some(Box::new(|ms| delay_calls.borrow_mut().push(ms))),
        unlink: Some(Box::new(|path: &Path| {
            *unlink_calls.borrow_mut() += 1;
            if *unlink_calls.borrow() < 3 {
                return Err(eperm());
            }
            fs::remove_file(path)
        })),
    };
    reap_stale_lock_with(&lock_path, &deps);
    assert_eq!(*unlink_calls.borrow(), 3);
    assert_eq!(*delay_calls.borrow(), vec![25, 25]);
    assert!(fs::read_to_string(&lock_path).is_err());
}

#[test]
fn atomic_write_syncs_temp_files_through_a_writable_handle() {
    let root = temp("locks-atomic-writable-");
    let target = root.path().join("target.txt");
    let open_flags = RefCell::new(Vec::new());
    let deps = AtomicWriteDeps {
        open: Some(Box::new(|path: &Path| {
            open_flags.borrow_mut().push("wx");
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
        })),
        ..AtomicWriteDeps::default()
    };
    atomic_write_with(&target, "new content", &deps).expect("write");
    assert_eq!(*open_flags.borrow(), vec!["wx"]);
    assert_eq!(fs::read_to_string(&target).expect("read"), "new content");
}

#[test]
fn detects_and_reaps_stale_lock_entries() {
    let root = temp("locks-stale-");
    let lock_path = root.path().join("lock");
    fs::write(
        &lock_path,
        format!("fake-owner-name\n999999999\n{}\n", now() - 600_000),
    )
    .expect("lock");
    let stale = detect_stale_lock(&lock_path, 300_000);
    reap_stale_lock(&lock_path);
    assert!(stale);
    assert!(fs::read_to_string(&lock_path).is_err());
}

// --- store.test.ts ---------------------------------------------------------------------------

fn store_config(base: &Path) -> TeamModeConfig {
    TeamModeConfig {
        max_members: 6,
        max_parallel_members: 3,
        max_messages_per_run: 200,
        max_wall_clock_minutes: 45,
        max_member_turns: 50,
        ..TeamModeConfig::with_base_dir(base.display().to_string())
    }
}

fn spec(name: &str) -> TeamSpec {
    let mut lead = Member::subagent("lead", "sisyphus");
    lead.color = Some("red".into());
    let mut worker = Member::category("worker", "deep", "implement task");
    worker.color = Some("blue".into());
    TeamSpec {
        version: 1,
        name: name.to_owned(),
        description: None,
        created_at: now(),
        lead_agent_id: "lead".into(),
        team_allowed_paths: None,
        session_permission: None,
        members: vec![lead, worker],
    }
}

fn random_spec() -> TeamSpec {
    spec(&format!("team-{}", &uuid::Uuid::new_v4().to_string()[..8]))
}

fn state_path(base: &Path, team_run_id: &str) -> PathBuf {
    base.join("runtime").join(team_run_id).join("state.json")
}

fn with_status(state: &RuntimeState, status: RuntimeStatus) -> RuntimeState {
    RuntimeState {
        status,
        ..state.clone()
    }
}

#[test]
fn create_runtime_state_persists_creating_state_with_computed_bounds() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let state =
        create_runtime_state(&random_spec(), None, SpecSource::User, &config).expect("create");
    let persisted: Value = serde_json::from_str(
        &fs::read_to_string(state_path(base.path(), &state.team_run_id)).expect("read"),
    )
    .expect("json");

    let parsed = uuid::Uuid::parse_str(&state.team_run_id).expect("uuid");
    assert!((1..=5).contains(&parsed.get_version_num()));
    assert_eq!(parsed.get_variant(), uuid::Variant::RFC4122);
    assert_eq!(state.status, RuntimeStatus::Creating);
    assert!(state.lead_session_id.is_none());
    assert_eq!(
        state.bounds,
        RuntimeBounds {
            max_members: 6,
            max_parallel_members: 3,
            max_messages_per_run: 200,
            max_wall_clock_minutes: 45,
            max_member_turns: 50,
        }
    );
    let members: Vec<(&str, AgentType, MemberStatus, usize)> = state
        .members
        .iter()
        .map(|member| {
            (
                member.name.as_str(),
                member.agent_type,
                member.status,
                member.pending_injected_message_ids.len(),
            )
        })
        .collect();
    assert_eq!(
        members,
        vec![
            ("lead", AgentType::Leader, MemberStatus::Pending, 0),
            (
                "worker",
                AgentType::GeneralPurpose,
                MemberStatus::Pending,
                0
            ),
        ]
    );
    assert_eq!(persisted["status"], "creating");
}

#[test]
fn load_runtime_state_throws_runtime_state_error_for_malformed_state() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let team_run_id = uuid::Uuid::new_v4().to_string();
    fs::create_dir_all(base.path().join("runtime").join(&team_run_id)).expect("dir");
    fs::write(state_path(base.path(), &team_run_id), "{not-json").expect("state");
    let error = load_runtime_state(&team_run_id, &config).expect_err("malformed");
    assert!(
        matches!(error, TeamCoreError::RuntimeState { .. }),
        "{error:?}"
    );
}

#[test]
fn transition_runtime_state_allows_active_to_shutdown_requested() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let created = create_runtime_state(
        &random_spec(),
        Some("lead-session"),
        SpecSource::Project,
        &config,
    )
    .expect("create");
    transition_runtime_state(
        &created.team_run_id,
        |state| with_status(&state, RuntimeStatus::Active),
        &config,
    )
    .expect("active");
    let state = transition_runtime_state(
        &created.team_run_id,
        |state| with_status(&state, RuntimeStatus::ShutdownRequested),
        &config,
    )
    .expect("shutdown_requested");
    assert_eq!(state.status, RuntimeStatus::ShutdownRequested);
    assert_eq!(
        load_runtime_state(&created.team_run_id, &config)
            .expect("load")
            .status,
        RuntimeStatus::ShutdownRequested
    );
}

#[test]
fn transition_runtime_state_rejects_reverse_transition() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let created =
        create_runtime_state(&random_spec(), None, SpecSource::User, &config).expect("create");
    save_runtime_state(&with_status(&created, RuntimeStatus::Deleted), &config)
        .expect("seed deleted");
    let error = transition_runtime_state(
        &created.team_run_id,
        |state| with_status(&state, RuntimeStatus::Active),
        &config,
    )
    .expect_err("reverse");
    assert!(
        matches!(error, TeamCoreError::InvalidTransition { .. }),
        "{error:?}"
    );
    assert_eq!(
        load_runtime_state(&created.team_run_id, &config)
            .expect("load")
            .status,
        RuntimeStatus::Deleted
    );
}

#[test]
fn load_runtime_state_ignores_crash_left_tmp_files() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let state =
        create_runtime_state(&random_spec(), None, SpecSource::User, &config).expect("create");
    let path = state_path(base.path(), &state.team_run_id);
    let crashed = serde_json::to_string(&with_status(&state, RuntimeStatus::Active)).expect("json");
    fs::write(format!("{}.tmp.mock-crash", path.display()), crashed).expect("tmp");
    assert_eq!(
        load_runtime_state(&state.team_run_id, &config)
            .expect("load")
            .status,
        RuntimeStatus::Creating
    );
}

#[test]
fn load_runtime_state_accepts_legacy_member_delegate_counters_without_preserving_them() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let state =
        create_runtime_state(&random_spec(), None, SpecSource::User, &config).expect("create");
    let mut raw = serde_json::to_value(&state).expect("json");
    for member in raw["members"].as_array_mut().expect("members") {
        member["delegateTaskCallsUsed"] = json!(3);
    }
    fs::write(state_path(base.path(), &state.team_run_id), raw.to_string()).expect("write");
    let persisted = load_runtime_state(&state.team_run_id, &config).expect("load");
    assert_eq!(persisted.members.len(), 2);
    let first = serde_json::to_value(&persisted.members[0]).expect("json");
    assert!(first.get("delegateTaskCallsUsed").is_none());
}

fn summary(
    state: &RuntimeState,
    name: &str,
    scope: SpecSource,
    lead_session_id: Option<&str>,
) -> ActiveTeamSummary {
    ActiveTeamSummary {
        team_run_id: state.team_run_id.clone(),
        team_name: name.to_owned(),
        status: RuntimeStatus::Creating,
        lead_session_id: lead_session_id.map(str::to_owned),
        member_count: 2,
        scope,
    }
}

#[test]
fn list_active_teams_skips_malformed_runtime_states() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let first =
        create_runtime_state(&spec("alpha-team"), None, SpecSource::User, &config).expect("alpha");
    let second =
        create_runtime_state(&spec("beta-team"), None, SpecSource::Project, &config).expect("beta");
    let malformed = uuid::Uuid::new_v4().to_string();
    fs::create_dir_all(base.path().join("runtime").join(&malformed)).expect("dir");
    fs::write(state_path(base.path(), &malformed), "{oops").expect("state");
    assert_eq!(
        list_active_teams(&config).expect("list"),
        vec![
            summary(&first, "alpha-team", SpecSource::User, None),
            summary(&second, "beta-team", SpecSource::Project, None),
        ]
    );
}

#[test]
fn list_active_teams_summary_carries_lead_session_id() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let state = create_runtime_state(
        &spec("lead-session-team"),
        Some("lead-session-14"),
        SpecSource::Project,
        &config,
    )
    .expect("create");
    let teams = list_active_teams(&config).expect("list");
    assert_eq!(
        teams.first(),
        Some(&summary(
            &state,
            "lead-session-team",
            SpecSource::Project,
            Some("lead-session-14")
        ))
    );
}

#[test]
fn list_active_teams_removes_deleted_runtime_directories() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let state = create_runtime_state(&spec("deleted-team"), None, SpecSource::User, &config)
        .expect("create");
    save_runtime_state(&with_status(&state, RuntimeStatus::Deleted), &config).expect("deleted");
    assert!(list_active_teams(&config).expect("list").is_empty());
    assert!(
        !base
            .path()
            .join("runtime")
            .join(&state.team_run_id)
            .exists()
    );
}

#[test]
fn list_active_teams_removes_deleting_runtimes_stuck_past_the_stale_timeout() {
    let base = temp("team-mode-store-");
    let config = store_config(base.path());
    let state = create_runtime_state(&spec("stuck-delete-team"), None, SpecSource::User, &config)
        .expect("create");
    save_runtime_state(&with_status(&state, RuntimeStatus::Deleting), &config).expect("deleting");
    let stale_ms = u64::try_from(STALE_DELETING_TTL_MS + 1_000).expect("ttl");
    let stale =
        filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_millis(stale_ms));
    filetime::set_file_times(state_path(base.path(), &state.team_run_id), stale, stale)
        .expect("utimes");
    assert!(list_active_teams(&config).expect("list").is_empty());
    assert!(
        !base
            .path()
            .join("runtime")
            .join(&state.team_run_id)
            .exists()
    );
}

// --- resume.test.ts --------------------------------------------------------------------------

struct MockSessions {
    alive: Vec<&'static str>,
    missing_data: bool,
    messages: Option<Value>,
    get_calls: Mutex<usize>,
}

impl MockSessions {
    fn none_found() -> Self {
        Self {
            alive: Vec::new(),
            missing_data: true,
            messages: None,
            get_calls: Mutex::new(0),
        }
    }

    fn alive(ids: &[&'static str]) -> Self {
        Self {
            alive: ids.to_vec(),
            missing_data: false,
            messages: None,
            get_calls: Mutex::new(0),
        }
    }

    fn all_alive() -> Self {
        Self {
            alive: vec!["*"],
            missing_data: false,
            messages: None,
            get_calls: Mutex::new(0),
        }
    }

    fn with_messages(mut self, messages: Value) -> Self {
        self.messages = Some(messages);
        self
    }

    fn get_calls(&self) -> usize {
        *self.get_calls.lock().expect("calls")
    }
}

impl TeamSessionClient for MockSessions {
    fn get(&self, session_id: &str) -> Result<SessionLookupResponse, SessionClientError> {
        *self.get_calls.lock().expect("calls") += 1;
        if self.missing_data {
            return Ok(SessionLookupResponse::default());
        }
        if self.alive.contains(&"*") || self.alive.contains(&session_id) {
            return Ok(SessionLookupResponse {
                data: Some(json!({ "id": session_id })),
                error: None,
            });
        }
        Err(SessionClientError {
            status: Some(404),
            message: Some("session not found".into()),
        })
    }

    fn messages(&self, _session_id: &str) -> Option<Result<Value, SessionClientError>> {
        self.messages.clone().map(Ok)
    }
}

fn resume(sessions: &MockSessions, config: &TeamModeConfig) -> ResumeReport {
    resume_all_teams(&TeamSessionContext { client: sessions }, config).expect("resume")
}

fn report_counts(report: &ResumeReport) -> (usize, usize, usize, usize, usize) {
    (
        report.resumed,
        report.marked_failed,
        report.marked_orphaned,
        report.cleaned,
        report.errors.len(),
    )
}

fn two_worker_spec() -> TeamSpec {
    let mut base = random_spec();
    let mut worker_a = Member::category("worker-a", "deep", "implement task");
    worker_a.color = Some("blue".into());
    let mut worker_b = Member::category("worker-b", "deep", "implement task");
    worker_b.color = Some("green".into());
    base.members.truncate(1);
    base.members.extend([worker_a, worker_b]);
    base
}

fn activate(
    team_run_id: &str,
    config: &TeamModeConfig,
    sessions: &[(&str, Option<&str>, MemberStatus)],
) {
    transition_runtime_state(
        team_run_id,
        |mut state| {
            state.status = RuntimeStatus::Active;
            if !sessions.is_empty() {
                state.lead_session_id = Some("ses_alive_lead".into());
            }
            for member in &mut state.members {
                if let Some((_, session_id, status)) =
                    sessions.iter().find(|(name, _, _)| *name == member.name)
                {
                    member.session_id = session_id.map(str::to_owned);
                    member.status = *status;
                }
            }
            state
        },
        config,
    )
    .expect("activate");
}

fn member<'a>(state: &'a RuntimeState, name: &str) -> &'a team_core::types::RuntimeStateMember {
    state
        .members
        .iter()
        .find(|member| member.name == name)
        .expect("member")
}

fn ancient_reservation(inbox: &Path, message_id: &str, body: &str) -> PathBuf {
    fs::create_dir_all(inbox).expect("inbox");
    let path = inbox.join(format!(".delivering-{message_id}.json"));
    let message = json!({
        "version": 1, "messageId": message_id, "from": "lead", "to": "worker",
        "kind": "message", "body": body, "timestamp": now(),
    });
    fs::write(&path, message.to_string()).expect("reservation");
    let ancient =
        filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(60 * 60));
    filetime::set_file_times(&path, ancient, ancient).expect("utimes");
    path
}

fn entries(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .expect("read_dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

fn worker_inbox(config: &TeamModeConfig, team_run_id: &str) -> PathBuf {
    get_inbox_dir(&resolve_base_dir(config), team_run_id, "worker").expect("inbox")
}

#[test]
fn resume_marks_stuck_creating_teams_failed_after_reload_recovery() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(&random_spec(), Some("ses_lead"), SpecSource::User, &config)
        .expect("create");
    let worktree_path = base
        .path()
        .join("worktrees")
        .join(&state.team_run_id)
        .join("worker");
    fs::create_dir_all(&worktree_path).expect("worktree");
    let mut stuck = state.clone();
    stuck.created_at = now() - 40 * 60 * 1000;
    for member in &mut stuck.members {
        if member.name == "worker" {
            member.worktree_path = Some(worktree_path.display().to_string());
        }
    }
    save_runtime_state(&stuck, &config).expect("save");

    let report = resume(&MockSessions::none_found(), &config);

    assert_eq!(
        load_runtime_state(&state.team_run_id, &config)
            .expect("load")
            .status,
        RuntimeStatus::Failed
    );
    assert_eq!(report_counts(&report), (0, 1, 0, 0, 0));
    assert!(!worktree_path.exists());
}

#[test]
fn resume_leaves_fresh_creating_teams_pending() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(&random_spec(), Some("ses_lead"), SpecSource::User, &config)
        .expect("create");
    let report = resume(&MockSessions::none_found(), &config);
    assert_eq!(
        load_runtime_state(&state.team_run_id, &config)
            .expect("load")
            .status,
        RuntimeStatus::Creating
    );
    assert_eq!(report_counts(&report), (0, 0, 0, 0, 0));
}

#[test]
fn resume_marks_active_teams_orphaned_when_lead_session_no_longer_exists() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(
        &random_spec(),
        Some("ses_dead"),
        SpecSource::Project,
        &config,
    )
    .expect("create");
    activate(&state.team_run_id, &config, &[]);
    let sessions = MockSessions::alive(&[]);
    let report = resume(&sessions, &config);
    assert_eq!(sessions.get_calls(), 1);
    assert_eq!(
        load_runtime_state(&state.team_run_id, &config)
            .expect("load")
            .status,
        RuntimeStatus::Orphaned
    );
    assert_eq!(report_counts(&report), (0, 0, 1, 0, 0));
}

#[test]
fn resume_preserves_active_teams_when_lead_session_is_still_alive() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(&random_spec(), Some("ses_alive"), SpecSource::User, &config)
        .expect("create");
    activate(&state.team_run_id, &config, &[]);
    let sessions = MockSessions::alive(&["ses_alive"]);
    let report = resume(&sessions, &config);
    assert_eq!(sessions.get_calls(), 1);
    assert_eq!(
        load_runtime_state(&state.team_run_id, &config)
            .expect("load")
            .status,
        RuntimeStatus::Active
    );
    assert_eq!(report_counts(&report), (1, 0, 0, 0, 0));
}

#[test]
fn resume_marks_dead_worker_members_errored_while_keeping_the_team_active() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(
        &two_worker_spec(),
        Some("ses_alive_lead"),
        SpecSource::User,
        &config,
    )
    .expect("create");
    activate(
        &state.team_run_id,
        &config,
        &[
            ("lead", Some("ses_alive_lead"), MemberStatus::Running),
            ("worker-a", Some("ses_dead_a"), MemberStatus::Running),
            ("worker-b", Some("ses_alive_b"), MemberStatus::Running),
        ],
    );
    let report = resume(
        &MockSessions::alive(&["ses_alive_lead", "ses_alive_b"]),
        &config,
    );
    let persisted = load_runtime_state(&state.team_run_id, &config).expect("load");
    assert_eq!(persisted.status, RuntimeStatus::Active);
    assert_eq!(member(&persisted, "worker-a").status, MemberStatus::Errored);
    assert!(member(&persisted, "worker-a").session_id.is_none());
    assert_eq!(member(&persisted, "worker-b").status, MemberStatus::Running);
    assert_eq!(
        member(&persisted, "worker-b").session_id.as_deref(),
        Some("ses_alive_b")
    );
    assert_eq!(report.resumed, 1);
    assert_eq!(report.marked_orphaned, 0);
}

#[test]
fn resume_reclaims_stale_delivering_reservations_of_an_active_team() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(&random_spec(), Some("ses_alive"), SpecSource::User, &config)
        .expect("create");
    activate(&state.team_run_id, &config, &[]);
    let inbox = worker_inbox(&config, &state.team_run_id);
    let stranded = uuid::Uuid::new_v4().to_string();
    ancient_reservation(&inbox, &stranded, "stranded");
    resume(&MockSessions::alive(&["ses_alive"]), &config);
    let names = entries(&inbox);
    assert!(names.contains(&format!("{stranded}.json")));
    assert!(!names.contains(&format!(".delivering-{stranded}.json")));
}

#[test]
fn resume_clears_pending_state_for_reclaimed_reservation_absent_from_session_history() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(
        &random_spec(),
        Some("ses_alive_lead"),
        SpecSource::User,
        &config,
    )
    .expect("create");
    let message_id = uuid::Uuid::new_v4().to_string();
    let pending = message_id.clone();
    transition_runtime_state(
        &state.team_run_id,
        move |mut state| {
            state.status = RuntimeStatus::Active;
            state.lead_session_id = Some("ses_alive_lead".into());
            for member in &mut state.members {
                member.status = MemberStatus::Running;
                if member.name == "lead" {
                    member.session_id = Some("ses_alive_lead".into());
                } else {
                    member.session_id = Some("ses_worker".into());
                    member.pending_injected_message_ids = vec![pending.clone()];
                }
            }
            state
        },
        &config,
    )
    .expect("activate");
    let inbox = worker_inbox(&config, &state.team_run_id);
    ancient_reservation(&inbox, &message_id, "retry after restart");

    resume(
        &MockSessions::all_alive().with_messages(json!({ "data": [] })),
        &config,
    );

    let names = entries(&inbox);
    assert!(names.contains(&format!("{message_id}.json")));
    assert!(!names.contains(&format!(".delivering-{message_id}.json")));
    let persisted = load_runtime_state(&state.team_run_id, &config).expect("load");
    assert!(
        member(&persisted, "worker")
            .pending_injected_message_ids
            .is_empty()
    );
}

#[test]
fn resume_removes_hidden_reservation_of_accepted_delivery_without_losing_the_message() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(
        &random_spec(),
        Some("ses_alive_lead"),
        SpecSource::User,
        &config,
    )
    .expect("create");
    activate(
        &state.team_run_id,
        &config,
        &[
            ("lead", Some("ses_alive_lead"), MemberStatus::Running),
            ("worker", Some("ses_worker"), MemberStatus::Running),
        ],
    );
    let message_id = uuid::Uuid::new_v4().to_string();
    let inbox = worker_inbox(&config, &state.team_run_id);
    ancient_reservation(&inbox, &message_id, "already accepted");
    let history = json!({ "data": [{
        "info": { "role": "user" },
        "parts": [{ "type": "text", "text": format!("<peer_message from=\"lead\" messageId=\"{message_id}\" kind=\"message\">already accepted</peer_message>") }],
    }] });

    resume(&MockSessions::all_alive().with_messages(history), &config);

    let names = entries(&inbox);
    assert!(!names.contains(&format!(".delivering-{message_id}.json")));
    if names.iter().any(|name| name == "processed") {
        assert!(entries(&inbox.join("processed")).contains(&format!("{message_id}.json")));
    } else {
        assert!(names.contains(&format!("{message_id}.json")));
    }
}

#[test]
fn resume_leaves_fresh_delivering_reservations_in_place() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(&random_spec(), Some("ses_alive"), SpecSource::User, &config)
        .expect("create");
    activate(&state.team_run_id, &config, &[]);
    let inbox = worker_inbox(&config, &state.team_run_id);
    fs::create_dir_all(&inbox).expect("inbox");
    let fresh = uuid::Uuid::new_v4().to_string();
    fs::write(inbox.join(format!(".delivering-{fresh}.json")), "{}").expect("fresh");
    resume(&MockSessions::alive(&["ses_alive"]), &config);
    let names = entries(&inbox);
    assert!(names.contains(&format!(".delivering-{fresh}.json")));
    assert!(!names.contains(&format!("{fresh}.json")));
}

#[test]
fn resume_orphans_active_teams_when_every_worker_session_has_died() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(
        &random_spec(),
        Some("ses_alive_lead"),
        SpecSource::User,
        &config,
    )
    .expect("create");
    activate(
        &state.team_run_id,
        &config,
        &[
            ("lead", Some("ses_alive_lead"), MemberStatus::Running),
            ("worker", Some("ses_dead_worker"), MemberStatus::Running),
        ],
    );
    let report = resume(&MockSessions::alive(&["ses_alive_lead"]), &config);
    let persisted = load_runtime_state(&state.team_run_id, &config).expect("load");
    assert_eq!(persisted.status, RuntimeStatus::Orphaned);
    assert_eq!(member(&persisted, "worker").status, MemberStatus::Errored);
    assert!(member(&persisted, "worker").session_id.is_none());
    assert_eq!(report.resumed, 0);
    assert_eq!(report.marked_orphaned, 1);
}

#[test]
fn resume_orphans_active_teams_when_last_live_worker_died_after_another_errored() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(
        &two_worker_spec(),
        Some("ses_alive_lead"),
        SpecSource::User,
        &config,
    )
    .expect("create");
    activate(
        &state.team_run_id,
        &config,
        &[
            ("lead", Some("ses_alive_lead"), MemberStatus::Running),
            ("worker-a", None, MemberStatus::Errored),
            ("worker-b", Some("ses_dead_b"), MemberStatus::Running),
        ],
    );
    let report = resume(&MockSessions::alive(&["ses_alive_lead"]), &config);
    let persisted = load_runtime_state(&state.team_run_id, &config).expect("load");
    assert_eq!(persisted.status, RuntimeStatus::Orphaned);
    assert_eq!(member(&persisted, "worker-a").status, MemberStatus::Errored);
    assert_eq!(member(&persisted, "worker-b").status, MemberStatus::Errored);
    assert!(member(&persisted, "worker-b").session_id.is_none());
    assert_eq!(report.resumed, 0);
    assert_eq!(report.marked_orphaned, 1);
}

#[test]
fn resume_finishes_deleting_teams_and_removes_the_runtime_directory() {
    let base = temp("team-mode-resume-");
    let config = store_config(base.path());
    let state = create_runtime_state(&random_spec(), Some("ses_lead"), SpecSource::User, &config)
        .expect("create");
    activate(&state.team_run_id, &config, &[]);
    transition_runtime_state(
        &state.team_run_id,
        |state| with_status(&state, RuntimeStatus::Deleting),
        &config,
    )
    .expect("deleting");
    let worktree_path = base
        .path()
        .join("worktrees")
        .join(&state.team_run_id)
        .join("worker");
    fs::create_dir_all(&worktree_path).expect("worktree");
    let mut deleting = load_runtime_state(&state.team_run_id, &config).expect("load");
    deleting.members = state
        .members
        .iter()
        .cloned()
        .map(|mut member| {
            if member.name == "worker" {
                member.worktree_path = Some(worktree_path.display().to_string());
            }
            member
        })
        .collect();
    save_runtime_state(&deleting, &config).expect("save");

    let report = resume(&MockSessions::none_found(), &config);

    assert_eq!(report_counts(&report), (0, 0, 0, 1, 0));
    let error = load_runtime_state(&state.team_run_id, &config).expect_err("gone");
    assert!(error.is_not_found(), "{error:?}");
    assert!(!worktree_path.exists());
}
