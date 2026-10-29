//! Translated from src/team-layout-tmux/*.test.ts.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use team_core::team_layout_tmux::{
    CloseTeamMemberPaneDeps, RebalanceLayout, RebalanceTeamWindowDeps, ResolvedCallerTmuxSession,
    TeamLayoutCleanupTarget, TeamLayoutDeps, TeamLayoutMember, TeamSweepDeps, TmuxSessionManager,
    close_team_member_pane_with, create_team_layout, rebalance_team_window_with,
    remove_team_layout, resolve_caller_tmux_session, sweep_stale_team_sessions_with,
};
use tmux_core::TmuxCommandResult;

type Calls = Arc<Mutex<Vec<Vec<String>>>>;
type Logs = Arc<Mutex<Vec<(String, Option<Value>)>>>;

fn tmux_result(output: &str, success: bool) -> TmuxCommandResult {
    TmuxCommandResult::new(
        output,
        if success { "" } else { "error" },
        i32::from(!success),
    )
}

fn sv(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

const CALLER_PANE: &str = "%42";

struct Harness {
    calls: Calls,
    logs: Logs,
    env: Arc<Mutex<HashMap<String, String>>>,
    server_running: Arc<Mutex<bool>>,
}

/// The `defaultRunTmuxCommand` mock: splits allocate `%N`, list-panes returns the caller pane.
fn default_run(
    state: &Mutex<(u32, HashMap<String, Vec<String>>)>,
    args: &[String],
) -> TmuxCommandResult {
    let mut state = state.lock().expect("state");
    match args[0].as_str() {
        "list-panes" => {
            let panes = state
                .1
                .get(&args[2])
                .cloned()
                .unwrap_or_else(|| vec![CALLER_PANE.to_owned()]);
            tmux_result(&panes.join("\n"), true)
        }
        "split-window" => {
            state.0 += 1;
            tmux_result(&format!("%{}", state.0), true)
        }
        _ => tmux_result("", true),
    }
}

type RunFn = Arc<dyn Fn(&[String]) -> Result<TmuxCommandResult, String> + Send + Sync>;

impl Harness {
    fn new() -> Self {
        let mut env = HashMap::new();
        env.insert("TMUX".to_owned(), "/tmp/tmux-1".to_owned());
        env.insert("TMUX_PANE".to_owned(), CALLER_PANE.to_owned());
        Self {
            calls: Arc::default(),
            logs: Arc::default(),
            env: Arc::new(Mutex::new(env)),
            server_running: Arc::new(Mutex::new(true)),
        }
    }

    fn deps_with(&self, run: RunFn) -> TeamLayoutDeps {
        let calls = Arc::clone(&self.calls);
        let logs = Arc::clone(&self.logs);
        let env = Arc::clone(&self.env);
        let resolve_env = Arc::clone(&self.env);
        let server_running = Arc::clone(&self.server_running);
        TeamLayoutDeps {
            run_tmux_command: Arc::new(move |_path, args| {
                calls.lock().expect("calls").push(args.to_vec());
                run(args)
            }),
            is_server_running: Arc::new(move |_url| *server_running.lock().expect("running")),
            get_tmux_path: Arc::new(|| Ok(Some("tmux".to_owned()))),
            resolve_caller_tmux_session: Arc::new(move |_path| {
                resolve_env
                    .lock()
                    .expect("env")
                    .get("TMUX_PANE")
                    .map(|pane| ResolvedCallerTmuxSession {
                        session_id: "$7".to_owned(),
                        pane_id: pane.clone(),
                        window_target: "test-session:0".to_owned(),
                    })
            }),
            log: Arc::new(move |message, data| {
                logs.lock().expect("logs").push((message.to_owned(), data))
            }),
            env: Arc::new(move |key| env.lock().expect("env").get(key).cloned()),
            cwd: Arc::new(|| "/cwd".to_owned()),
        }
    }

    fn deps(&self) -> TeamLayoutDeps {
        let state = Arc::new(Mutex::new((0_u32, HashMap::new())));
        self.deps_with(Arc::new(move |args| Ok(default_run(&state, args))))
    }

    fn commands(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls").clone()
    }

    fn commands_named(&self, name: &str) -> Vec<Vec<String>> {
        self.commands()
            .into_iter()
            .filter(|args| args[0] == name)
            .collect()
    }
}

struct Mgr;
impl TmuxSessionManager for Mgr {
    fn get_server_url(&self) -> String {
        "http://127.0.0.1:12345".to_owned()
    }
}

fn members(count: usize) -> Vec<TeamLayoutMember> {
    (1..=count)
        .map(|index| TeamLayoutMember {
            name: format!("m{index}"),
            session_id: format!("s-m{index}"),
            worktree_path: Some(format!("/tmp/m{index}")),
        })
        .collect()
}

fn last_args(commands: &[Vec<String>]) -> Vec<String> {
    commands
        .iter()
        .map(|args| args.last().cloned().unwrap_or_default())
        .collect()
}

// --- layout.test.ts --------------------------------------------------------------------------

#[test]
fn layout_returns_null_and_makes_no_tmux_calls_when_visualization_unavailable() {
    let harness = Harness::new();
    harness.env.lock().expect("env").remove("TMUX");
    let result = create_team_layout("run-1", &[], &Mgr, &harness.deps());
    assert!(result.is_none());
    assert!(harness.commands().is_empty());
}

#[test]
fn layout_returns_null_when_server_health_check_fails() {
    let harness = Harness::new();
    *harness.server_running.lock().expect("running") = false;
    let lead = TeamLayoutMember {
        name: "lead".into(),
        session_id: "s1".into(),
        worktree_path: Some("/tmp/lead".into()),
    };
    assert!(create_team_layout("run-health", &[lead], &Mgr, &harness.deps()).is_none());
    assert!(harness.commands().is_empty());
}

#[test]
fn layout_given_tmux_path_lookup_throws_non_error_falls_back_to_null() {
    let harness = Harness::new();
    let mut deps = harness.deps();
    deps.get_tmux_path = Arc::new(|| Err("tmux path unavailable".to_owned()));
    assert!(create_team_layout("run-non-error", &members(1), &Mgr, &deps).is_none());
    assert!(harness.logs.lock().expect("logs").contains(&(
        "tmux visualization unavailable, skipping".to_owned(),
        Some(json!({ "error": "tmux path unavailable" }))
    )));
}

#[test]
fn layout_creates_teammate_panes_in_caller_window_and_sends_attach_via_send_keys() {
    let harness = Harness::new();
    create_team_layout("run-attach", &members(2), &Mgr, &harness.deps());
    assert!(harness.commands_named("new-window").is_empty());
    assert_eq!(harness.commands_named("split-window").len(), 2);
    let literals: Vec<String> = harness
        .commands_named("send-keys")
        .iter()
        .map(|args| args.join(" "))
        .collect();
    assert!(
        literals
            .iter()
            .any(|line| line.contains("--session 's-m1'"))
    );
    assert!(
        literals
            .iter()
            .any(|line| line.contains("--session 's-m2'"))
    );
}

#[test]
fn layout_given_env_auth_send_keys_omit_secrets_while_split_window_forwards_pane_env() {
    let harness = Harness::new();
    let password = ["a", "'", "b"].concat();
    let username = "u".to_owned();
    {
        let mut env = harness.env.lock().expect("env");
        env.insert("OPENCODE_SERVER_PASSWORD".into(), password.clone());
        env.insert("OPENCODE_SERVER_USERNAME".into(), username.clone());
    }
    create_team_layout("run-auth", &members(1), &Mgr, &harness.deps());
    let send_keys: Vec<String> = harness
        .commands_named("send-keys")
        .iter()
        .map(|args| args.join(" "))
        .collect();
    let attach = send_keys
        .iter()
        .find(|line| line.contains("opencode attach"))
        .expect("attach command");
    assert!(!attach.contains("OPENCODE_SERVER_PASSWORD"));
    assert!(!attach.contains("OPENCODE_SERVER_USERNAME"));
    assert!(!attach.contains(&password));
    assert!(!attach.contains(&username));
    let split = harness
        .commands_named("split-window")
        .into_iter()
        .next()
        .expect("split-window");
    assert!(split.contains(&"-e".to_owned()));
    assert!(split.contains(&format!("OPENCODE_SERVER_PASSWORD={password}")));
    assert!(split.contains(&format!("OPENCODE_SERVER_USERNAME={username}")));
}

#[test]
fn layout_given_no_password_attach_command_has_no_env_prefix() {
    let harness = Harness::new();
    create_team_layout("run-noauth", &members(1), &Mgr, &harness.deps());
    let send_keys: Vec<String> = harness
        .commands_named("send-keys")
        .iter()
        .map(|args| args.join(" "))
        .collect();
    let attach = send_keys
        .iter()
        .find(|line| line.contains("opencode attach"))
        .expect("attach command");
    assert!(!attach.contains("OPENCODE_SERVER_PASSWORD"));
}

#[test]
fn layout_uses_caller_window_main_vertical_layout_with_caller_pane_as_primary() {
    let harness = Harness::new();
    let result =
        create_team_layout("run-layout", &members(3), &Mgr, &harness.deps()).expect("layout");
    let layouts = last_args(&harness.commands_named("select-layout"));
    assert!(layouts.contains(&"main-vertical".to_owned()));
    assert!(!layouts.contains(&"tiled".to_owned()));
    assert!(
        harness
            .commands()
            .contains(&sv(&["resize-pane", "-t", CALLER_PANE, "-x", "30%"]))
    );
    assert_eq!(
        result
            .focus_panes_by_member
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["m1", "m2", "m3"]
    );
    assert!(result.grid_panes_by_member.is_empty());
}

#[test]
fn layout_given_4_or_more_teammates_keeps_every_teammate_in_caller_window() {
    let harness = Harness::new();
    create_team_layout("run-tiled", &members(5), &Mgr, &harness.deps());
    assert!(harness.commands_named("new-window").is_empty());
    assert_eq!(harness.commands_named("split-window").len(), 5);
    let layouts = last_args(&harness.commands_named("select-layout"));
    assert!(layouts.contains(&"main-vertical".to_owned()));
    assert!(!layouts.contains(&"tiled".to_owned()));
}

#[test]
fn layout_never_steals_focus_or_mutates_window_border_options() {
    let harness = Harness::new();
    create_team_layout("run-no-focus", &members(5), &Mgr, &harness.deps());
    let commands = harness.commands();
    assert!(
        !commands
            .iter()
            .any(|args| args[0] == "select-pane" && !args.contains(&"-T".to_owned()))
    );
    assert!(
        !commands
            .iter()
            .any(|args| args[0] == "set-option" && args[1] != "-p")
    );
}

#[test]
fn layout_split_window_always_carries_detach_flag() {
    let harness = Harness::new();
    create_team_layout("run-probe-drain", &members(3), &Mgr, &harness.deps());
    let splits = harness.commands_named("split-window");
    assert!(!splits.is_empty());
    for split in splits {
        assert!(split.contains(&"-d".to_owned()), "{split:?}");
    }
}

fn caller_target() -> TeamLayoutCleanupTarget {
    TeamLayoutCleanupTarget {
        owned_session: false,
        target_session_id: "$caller".into(),
        ..TeamLayoutCleanupTarget::default()
    }
}

#[test]
fn remove_layout_not_owned_kills_both_windows_and_never_the_session() {
    let harness = Harness::new();
    let target = TeamLayoutCleanupTarget {
        focus_window_id: Some("@10".into()),
        grid_window_id: Some("@11".into()),
        ..caller_target()
    };
    remove_team_layout("run-cleanup", Some(&target), &harness.deps());
    let commands = harness.commands();
    assert!(commands.contains(&sv(&["kill-window", "-t", "@10"])));
    assert!(commands.contains(&sv(&["kill-window", "-t", "@11"])));
    assert!(harness.commands_named("kill-session").is_empty());
}

#[test]
fn remove_layout_not_owned_with_pane_ids_kills_panes_instead_of_caller_window() {
    let harness = Harness::new();
    let target = TeamLayoutCleanupTarget {
        focus_window_id: Some("test-session:0".into()),
        pane_ids: Some(sv(&["%10", "%11"])),
        ..caller_target()
    };
    remove_team_layout("run-cleanup", Some(&target), &harness.deps());
    let commands = harness.commands();
    assert!(commands.contains(&sv(&["kill-pane", "-t", "%10"])));
    assert!(commands.contains(&sv(&["kill-pane", "-t", "%11"])));
    assert!(harness.commands_named("kill-window").is_empty());
    assert!(harness.commands_named("kill-session").is_empty());
}

#[test]
fn remove_layout_owned_session_kills_target_session() {
    let harness = Harness::new();
    let target = TeamLayoutCleanupTarget {
        owned_session: true,
        target_session_id: "omo-team-xyz".into(),
        focus_window_id: Some("@10".into()),
        grid_window_id: Some("@11".into()),
        pane_ids: None,
    };
    remove_team_layout("run-cleanup", Some(&target), &harness.deps());
    assert!(
        harness
            .commands()
            .contains(&sv(&["kill-session", "-t", "omo-team-xyz"]))
    );
}

#[test]
fn remove_layout_first_kill_window_fails_second_still_fires() {
    let harness = Harness::new();
    let kill_windows = Arc::new(Mutex::new(0));
    let deps = harness.deps_with(Arc::new(move |args| {
        if args[0] == "kill-window" {
            let mut count = kill_windows.lock().expect("count");
            *count += 1;
            return Ok(tmux_result("", *count > 1));
        }
        Ok(tmux_result("", true))
    }));
    let target = TeamLayoutCleanupTarget {
        focus_window_id: Some("@10".into()),
        grid_window_id: Some("@11".into()),
        ..caller_target()
    };
    remove_team_layout("run-cleanup", Some(&target), &deps);
    assert_eq!(
        harness.commands_named("kill-window"),
        vec![
            sv(&["kill-window", "-t", "@10"]),
            sv(&["kill-window", "-t", "@11"])
        ]
    );
}

#[test]
fn remove_layout_pane_cleanup_throwing_non_error_continues() {
    let harness = Harness::new();
    let deps = harness.deps_with(Arc::new(|args| {
        if args[0] == "kill-pane" {
            return Err("pane already gone".to_owned());
        }
        Ok(tmux_result("", true))
    }));
    let target = TeamLayoutCleanupTarget {
        pane_ids: Some(sv(&["%11", "%12"])),
        ..caller_target()
    };
    remove_team_layout("run-pane-cleanup", Some(&target), &deps);
    assert_eq!(
        harness.commands_named("kill-pane"),
        vec![
            sv(&["kill-pane", "-t", "%11"]),
            sv(&["kill-pane", "-t", "%12"])
        ]
    );
    let logs = harness.logs.lock().expect("logs");
    for pane in ["%11", "%12"] {
        assert!(logs.contains(&(
            "tmux team pane cleanup failed".to_owned(),
            Some(json!({ "teamRunId": "run-pane-cleanup", "paneId": pane }))
        )));
    }
}

#[test]
fn layout_skips_all_panes_when_lead_member_missing() {
    let harness = Harness::new();
    assert!(create_team_layout("run-empty", &[], &Mgr, &harness.deps()).is_none());
    assert!(harness.commands_named("new-window").is_empty());
}

#[test]
fn topology_uses_the_caller_window_without_a_new_session() {
    let harness = Harness::new();
    create_team_layout("run-split", &members(2), &Mgr, &harness.deps());
    assert!(harness.commands_named("new-session").is_empty());
    assert!(harness.commands_named("new-window").is_empty());
    assert!(
        harness
            .commands_named("split-window")
            .iter()
            .any(|args| args.contains(&CALLER_PANE.to_owned()))
    );
}

#[test]
fn topology_caller_session_resolved_owned_session_is_false() {
    let harness = Harness::new();
    let result =
        create_team_layout("run-owned", &members(1), &Mgr, &harness.deps()).expect("layout");
    assert!(!result.owned_session);
}

#[test]
fn topology_first_teammate_splits_caller_pane_horizontally() {
    let harness = Harness::new();
    create_team_layout("run-first", &members(1), &Mgr, &harness.deps());
    assert_eq!(
        harness.commands_named("split-window"),
        vec![sv(&[
            "split-window",
            "-t",
            CALLER_PANE,
            "-h",
            "-d",
            "-l",
            "70%",
            "-P",
            "-F",
            "#{pane_id}",
            "-c",
            "/tmp/m1"
        ])]
    );
    assert!(harness.commands_named("new-window").is_empty());
}

#[test]
fn topology_three_members_get_three_distinct_pane_ids() {
    let harness = Harness::new();
    let result =
        create_team_layout("run-3-members", &members(3), &Mgr, &harness.deps()).expect("layout");
    assert_eq!(
        result
            .focus_panes_by_member
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["m1", "m2", "m3"]
    );
    assert_eq!(
        result
            .focus_panes_by_member
            .values()
            .collect::<HashSet<_>>()
            .len(),
        3
    );
}

#[test]
fn topology_layout_records_focus_panes_only() {
    let harness = Harness::new();
    let result =
        create_team_layout("run-layout", &members(2), &Mgr, &harness.deps()).expect("layout");
    assert_eq!(
        result
            .focus_panes_by_member
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["m1", "m2"]
    );
    assert!(result.grid_panes_by_member.is_empty());
    assert_eq!(result.focus_window_id, "test-session:0");
    assert!(result.grid_window_id.is_none());
    assert!(harness.commands_named("new-window").is_empty());
    assert!(
        harness
            .commands_named("send-keys")
            .iter()
            .any(|args| args.contains(&"Enter".to_owned()))
    );
}

// --- close-team-member-pane.test.ts ----------------------------------------------------------

fn close_with(results: Vec<bool>, pane: Option<&str>, grid: Option<&str>) -> (bool, Vec<String>) {
    let calls = Mutex::new(Vec::new());
    let results = Mutex::new(results);
    let close = |pane: &str| {
        calls.lock().expect("calls").push(pane.to_owned());
        let mut results = results.lock().expect("results");
        Ok(if results.len() > 1 {
            results.remove(0)
        } else {
            results[0]
        })
    };
    let deps = CloseTeamMemberPaneDeps {
        close_tmux_pane: &close,
        log: &|_, _| {},
    };
    let result = close_team_member_pane_with(pane, grid, &deps);
    (result, calls.into_inner().expect("calls"))
}

#[test]
fn close_pane_both_ids_closes_both_and_returns_true_when_either_succeeds() {
    let (result, calls) = close_with(vec![false, true], Some("%42"), Some("%84"));
    assert!(result);
    assert_eq!(calls, sv(&["%42", "%84"]));
}

#[test]
fn close_pane_only_pane_id_closes_once() {
    let (result, calls) = close_with(vec![true], Some("%42"), None);
    assert!(result);
    assert_eq!(calls, sv(&["%42"]));
}

#[test]
fn close_pane_both_closes_fail_returns_false() {
    let (result, calls) = close_with(vec![false], Some("%42"), Some("%84"));
    assert!(!result);
    assert_eq!(calls.len(), 2);
}

// --- rebalance-team-window.test.ts -----------------------------------------------------------

fn rebalance(
    window: &str,
    layout: RebalanceLayout,
    success: bool,
) -> (bool, Vec<Vec<String>>, usize) {
    let calls = Mutex::new(Vec::new());
    let logs = Mutex::new(0);
    let run = |args: &[String]| {
        calls.lock().expect("calls").push(args.to_vec());
        success
    };
    let log = |_: &str, _: Option<Value>| *logs.lock().expect("logs") += 1;
    let result = rebalance_team_window_with(
        window,
        layout,
        &RebalanceTeamWindowDeps {
            run_tmux: &run,
            log: &log,
        },
    );
    (
        result,
        calls.into_inner().expect("calls"),
        logs.into_inner().expect("logs"),
    )
}

#[test]
fn rebalance_main_vertical_selects_sets_width_and_reselects() {
    let (result, calls, _) = rebalance("@1", RebalanceLayout::MainVertical, true);
    assert!(result);
    assert_eq!(
        calls,
        vec![
            sv(&["select-layout", "-t", "@1", "main-vertical"]),
            sv(&["set-window-option", "-t", "@1", "main-pane-width", "60%"]),
            sv(&["select-layout", "-t", "@1", "main-vertical"]),
        ]
    );
}

#[test]
fn rebalance_focus_window_after_shrink_uses_main_vertical() {
    let (result, calls, _) = rebalance("@focus", RebalanceLayout::MainVertical, true);
    assert!(result);
    assert_eq!(
        calls,
        vec![
            sv(&["select-layout", "-t", "@focus", "main-vertical"]),
            sv(&[
                "set-window-option",
                "-t",
                "@focus",
                "main-pane-width",
                "60%"
            ]),
            sv(&["select-layout", "-t", "@focus", "main-vertical"]),
        ]
    );
}

#[test]
fn rebalance_tiled_only_selects_layout() {
    let (result, calls, _) = rebalance("@1", RebalanceLayout::Tiled, true);
    assert!(result);
    assert_eq!(calls, vec![sv(&["select-layout", "-t", "@1", "tiled"])]);
}

#[test]
fn rebalance_select_layout_fails_returns_false_and_logs_once() {
    let (result, _, logs) = rebalance("@1", RebalanceLayout::MainVertical, false);
    assert!(!result);
    assert_eq!(logs, 1);
}

// --- resolve-caller-tmux-session.test.ts -----------------------------------------------------

fn resolve_with(
    results: Vec<TmuxCommandResult>,
    pane: &str,
) -> (
    Option<ResolvedCallerTmuxSession>,
    Vec<(String, Vec<String>)>,
) {
    let calls = Mutex::new(Vec::new());
    let results = Mutex::new(results);
    let run = |path: &str, args: &[String]| {
        calls
            .lock()
            .expect("calls")
            .push((path.to_owned(), args.to_vec()));
        let mut results = results.lock().expect("results");
        if results.is_empty() {
            TmuxCommandResult::new("", "", 1)
        } else {
            results.remove(0)
        }
    };
    let result = resolve_caller_tmux_session("tmux", Some(pane), &run);
    (result, calls.into_inner().expect("calls"))
}

#[test]
fn resolve_caller_tmux_pane_unset_returns_null_without_calls() {
    let (result, calls) = resolve_with(vec![tmux_result("$7", true)], "");
    assert!(result.is_none());
    assert!(calls.is_empty());
}

#[test]
fn resolve_caller_display_returns_session_and_window() {
    let (result, calls) = resolve_with(
        vec![tmux_result("$7", true), tmux_result("test-session:0", true)],
        "%42",
    );
    assert_eq!(
        result,
        Some(ResolvedCallerTmuxSession {
            session_id: "$7".into(),
            pane_id: "%42".into(),
            window_target: "test-session:0".into()
        })
    );
    assert_eq!(
        calls,
        vec![
            (
                "tmux".to_owned(),
                sv(&["display", "-p", "-F", "#{session_id}", "-t", "%42"])
            ),
            (
                "tmux".to_owned(),
                sv(&[
                    "display",
                    "-p",
                    "-F",
                    "#{session_name}:#{window_index}",
                    "-t",
                    "%42"
                ])
            ),
        ]
    );
}

#[test]
fn resolve_caller_garbage_session_returns_null() {
    assert!(
        resolve_with(vec![tmux_result("garbage", true)], "%42")
            .0
            .is_none()
    );
}

#[test]
fn resolve_caller_display_non_success_returns_null() {
    assert!(
        resolve_with(vec![TmuxCommandResult::new("$7", "", 1)], "%42")
            .0
            .is_none()
    );
}

// --- sweep-stale-team-sessions.test.ts -------------------------------------------------------

struct SweepFixture {
    deps: TeamSweepDeps,
    killed: Arc<Mutex<Vec<String>>>,
    kill_calls: Arc<Mutex<usize>>,
    logs: Arc<Mutex<Vec<String>>>,
}

fn sweep_fixture(
    candidates: &[&str],
    fail_list: bool,
    fail_kill: Option<&'static str>,
) -> SweepFixture {
    let candidates = sv(candidates);
    let killed: Arc<Mutex<Vec<String>>> = Arc::default();
    let kill_calls: Arc<Mutex<usize>> = Arc::default();
    let logs: Arc<Mutex<Vec<String>>> = Arc::default();
    let (killed_in, calls_in, logs_in) = (
        Arc::clone(&killed),
        Arc::clone(&kill_calls),
        Arc::clone(&logs),
    );
    SweepFixture {
        deps: TeamSweepDeps {
            list_candidates: Arc::new(move || {
                if fail_list {
                    Err("list failed".into())
                } else {
                    Ok(candidates.clone())
                }
            }),
            kill_session: Arc::new(move |name| {
                *calls_in.lock().expect("calls") += 1;
                if fail_kill == Some(name) {
                    return Err("kill failed".into());
                }
                killed_in.lock().expect("killed").push(name.to_owned());
                Ok(())
            }),
            log: Arc::new(move |message, _| logs_in.lock().expect("logs").push(message.to_owned())),
        },
        killed,
        kill_calls,
        logs,
    }
}

fn ids(values: &[&str]) -> HashSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

const T1: &str = "omo-team-11111111-1111-1111-1111-111111111111";
const T2: &str = "omo-team-22222222-2222-2222-2222-222222222222";
const T3: &str = "omo-team-33333333-3333-3333-3333-333333333333";

#[test]
fn sweep_kills_only_sessions_whose_run_id_is_not_active() {
    let fixture = sweep_fixture(&[T1, T2, T3, "main", "omo-agents-123"], false, None);
    let result = sweep_stale_team_sessions_with(
        &ids(&["11111111-1111-1111-1111-111111111111"]),
        &fixture.deps,
    );
    assert_eq!(*fixture.kill_calls.lock().expect("calls"), 2);
    assert_eq!(*fixture.killed.lock().expect("killed"), sv(&[T2, T3]));
    assert_eq!(result, sv(&[T2, T3]));
}

#[test]
fn sweep_all_candidates_active_kills_none() {
    let fixture = sweep_fixture(&[T1, T2], false, None);
    let active = ids(&[
        "11111111-1111-1111-1111-111111111111",
        "22222222-2222-2222-2222-222222222222",
    ]);
    assert!(sweep_stale_team_sessions_with(&active, &fixture.deps).is_empty());
    assert_eq!(*fixture.kill_calls.lock().expect("calls"), 0);
}

#[test]
fn sweep_list_candidates_throws_returns_empty_and_logs() {
    let fixture = sweep_fixture(&[], true, None);
    assert!(sweep_stale_team_sessions_with(&HashSet::new(), &fixture.deps).is_empty());
    let logs = fixture.logs.lock().expect("logs");
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains("failed to list"));
}

#[test]
fn sweep_kill_session_throws_for_one_continues() {
    let fixture = sweep_fixture(&[T1, T2, T3], false, Some(T2));
    let result = sweep_stale_team_sessions_with(&HashSet::new(), &fixture.deps);
    assert_eq!(*fixture.kill_calls.lock().expect("calls"), 3);
    assert_eq!(*fixture.killed.lock().expect("killed"), sv(&[T1, T3]));
    assert_eq!(fixture.logs.lock().expect("logs").len(), 1);
    assert_eq!(result, sv(&[T1, T3]));
}

#[test]
fn sweep_empty_suffix_candidate_is_skipped() {
    let fixture = sweep_fixture(&["omo-team-", T1], false, None);
    let result = sweep_stale_team_sessions_with(&HashSet::new(), &fixture.deps);
    assert_eq!(*fixture.killed.lock().expect("killed"), sv(&[T1]));
    assert_eq!(result, sv(&[T1]));
}

#[test]
fn sweep_no_team_candidates_returns_empty_without_kills() {
    let fixture = sweep_fixture(&["main", "dev-shell", "project-grid"], false, None);
    assert!(sweep_stale_team_sessions_with(&ids(&["still-active-run"]), &fixture.deps).is_empty());
    assert_eq!(*fixture.kill_calls.lock().expect("calls"), 0);
}

#[test]
fn sweep_preserves_non_uuid_team_like_session_names() {
    let fixture = sweep_fixture(&["main", "omo-team-de2e", "dev-shell"], false, None);
    assert!(sweep_stale_team_sessions_with(&HashSet::new(), &fixture.deps).is_empty());
    assert_eq!(*fixture.kill_calls.lock().expect("calls"), 0);
    assert!(fixture.killed.lock().expect("killed").is_empty());
}
