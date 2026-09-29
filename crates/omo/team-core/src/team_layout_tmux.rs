//! Team visualization in tmux: caller-window layout, cleanup, pane close, rebalance and stale-session sweep.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde_json::{Value, json};
use tmux_core::{
    IsServerRunningOptions, RunTmuxOptions, SweepTmuxSessionsDeps, SweepTmuxSessionsOptions,
    TmuxCommandResult, close_tmux_pane, is_server_running, run_tmux_command,
    sweep_tmux_sessions_with,
};

use crate::logger;
use crate::shell_quote::shell_single_quote;
use crate::types::RuntimeStateMember;

const TEAM_PANE_TITLE_PREFIX: &str = "omo-team-";
const OMO_ATTACH_SERVER_URL_OPTION: &str = "@omo_attach_server_url";
const OMO_ATTACH_SESSION_ID_OPTION: &str = "@omo_attach_session_id";

/// Environment lookup (`process.env[key]`).
pub type EnvFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// `(message, data)` logger.
pub type LayoutLogFn = Arc<dyn Fn(&str, Option<Value>) + Send + Sync>;
/// `runTmuxCommand(tmuxPath, args)`; `Err` mirrors a rejected promise.
pub type LayoutRunTmuxFn =
    Arc<dyn Fn(&str, &[String]) -> Result<TmuxCommandResult, String> + Send + Sync>;

type IsServerRunningFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;
type GetTmuxPathFn = Arc<dyn Fn() -> Result<Option<String>, String> + Send + Sync>;
type ResolveCallerFn = Arc<dyn Fn(&str) -> Option<ResolvedCallerTmuxSession> + Send + Sync>;
type CwdFn = Arc<dyn Fn() -> String + Send + Sync>;
type CallerWindowLayout = (String, BTreeMap<String, String>);
type ListCandidatesFn = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;
type KillTeamSessionFn = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
type SweepListFn = Arc<dyn Fn(&str) -> Result<Vec<String>, String> + Send + Sync>;
type SweepKillFn = Arc<dyn Fn(&str) -> Result<bool, String> + Send + Sync>;

fn process_env() -> EnvFn {
    Arc::new(|key| std::env::var(key).ok())
}

fn default_log() -> LayoutLogFn {
    Arc::new(logger::log)
}

/// A layout member (`TeamLayoutMember`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamLayoutMember {
    pub name: String,
    pub session_id: String,
    pub worktree_path: Option<String>,
}

/// `TmuxSessionManager`: where the opencode server lives.
pub trait TmuxSessionManager {
    fn get_server_url(&self) -> String;
    fn get_ctx_server_url(&self) -> Option<String> {
        None
    }
}

/// `resolveCallerTmuxSession` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCallerTmuxSession {
    pub session_id: String,
    pub pane_id: String,
    pub window_target: String,
}

/// `TeamLayoutDeps` (plus the process environment and cwd, injected for tests).
#[derive(Clone)]
pub struct TeamLayoutDeps {
    pub run_tmux_command: LayoutRunTmuxFn,
    pub is_server_running: IsServerRunningFn,
    /// `Err` mirrors a rejected `getTmuxPath()`.
    pub get_tmux_path: GetTmuxPathFn,
    pub resolve_caller_tmux_session: ResolveCallerFn,
    pub log: LayoutLogFn,
    pub env: EnvFn,
    pub cwd: CwdFn,
}

impl Default for TeamLayoutDeps {
    fn default() -> Self {
        Self {
            run_tmux_command: Arc::new(|tmux_path, args| {
                Ok(run_tmux_command(
                    tmux_path,
                    args,
                    &RunTmuxOptions::default(),
                ))
            }),
            is_server_running: Arc::new(|url| {
                is_server_running(url, &mut IsServerRunningOptions::default())
            }),
            get_tmux_path: Arc::new(|| Ok(Some("tmux".to_owned()))),
            resolve_caller_tmux_session: Arc::new(|tmux_path| {
                resolve_caller_tmux_session(
                    tmux_path,
                    std::env::var("TMUX_PANE").ok().as_deref(),
                    &|path, args| run_tmux_command(path, args, &RunTmuxOptions::default()),
                )
            }),
            log: default_log(),
            env: process_env(),
            cwd: Arc::new(|| {
                std::env::current_dir()
                    .map(|dir| dir.display().to_string())
                    .unwrap_or_default()
            }),
        }
    }
}

/// `TeamLayoutResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamLayoutResult {
    pub focus_window_id: String,
    pub grid_window_id: Option<String>,
    pub focus_panes_by_member: BTreeMap<String, String>,
    pub grid_panes_by_member: BTreeMap<String, String>,
    pub target_session_id: String,
    pub owned_session: bool,
}

/// `TeamLayoutCleanupTarget`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TeamLayoutCleanupTarget {
    pub owned_session: bool,
    pub target_session_id: String,
    pub focus_window_id: Option<String>,
    pub grid_window_id: Option<String>,
    pub pane_ids: Option<Vec<String>>,
}

/// `canVisualize`: true when running inside tmux (`$TMUX` set).
#[must_use]
pub fn can_visualize() -> bool {
    can_visualize_with(&process_env())
}

fn can_visualize_with(env: &EnvFn) -> bool {
    env("TMUX").is_some()
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn pane_working_directory(member: &TeamLayoutMember, deps: &TeamLayoutDeps) -> String {
    member.worktree_path.clone().unwrap_or_else(|| (deps.cwd)())
}

fn build_attach_command(
    member: &TeamLayoutMember,
    server_url: &str,
    deps: &TeamLayoutDeps,
) -> String {
    format!(
        "opencode attach {} --session {} --dir {}",
        shell_single_quote(server_url),
        shell_single_quote(&member.session_id),
        shell_single_quote(&pane_working_directory(member, deps))
    )
}

fn build_pane_environment_args(deps: &TeamLayoutDeps) -> Vec<String> {
    let Some(password) = (deps.env)("OPENCODE_SERVER_PASSWORD").filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };
    let mut args = vec![
        "-e".to_owned(),
        format!("OPENCODE_SERVER_PASSWORD={password}"),
    ];
    if let Some(username) = (deps.env)("OPENCODE_SERVER_USERNAME") {
        args.push("-e".to_owned());
        args.push(format!("OPENCODE_SERVER_USERNAME={username}"));
    }
    args
}

fn list_panes_in_window(
    tmux_path: &str,
    window_target: &str,
    deps: &TeamLayoutDeps,
) -> Result<Vec<String>, String> {
    let result = (deps.run_tmux_command)(
        tmux_path,
        &strings(&["list-panes", "-t", window_target, "-F", "#{pane_id}"]),
    )?;
    if !result.success || result.output.is_empty() {
        return Ok(Vec::new());
    }
    Ok(result
        .output
        .trim()
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn select_existing_teammate_pane(teammate_panes: &[String], caller_pane_id: &str) -> String {
    teammate_panes
        .get(teammate_panes.len() / 2)
        .or_else(|| teammate_panes.last())
        .cloned()
        .unwrap_or_else(|| caller_pane_id.to_owned())
}

fn build_split_args(
    caller_pane_id: &str,
    teammate_panes: &[String],
    member: &TeamLayoutMember,
    deps: &TeamLayoutDeps,
) -> Vec<String> {
    let mut args = vec!["split-window".to_owned()];
    args.extend(build_pane_environment_args(deps));
    let directory = pane_working_directory(member, deps);
    if teammate_panes.is_empty() {
        args.extend(strings(&[
            "-t",
            caller_pane_id,
            "-h",
            "-d",
            "-l",
            "70%",
            "-P",
            "-F",
            "#{pane_id}",
            "-c",
            &directory,
        ]));
        return args;
    }
    let target = select_existing_teammate_pane(teammate_panes, caller_pane_id);
    let direction = if teammate_panes.len() % 2 == 1 {
        "-v"
    } else {
        "-h"
    };
    args.extend(strings(&[
        "-t",
        &target,
        direction,
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-c",
        &directory,
    ]));
    args
}

fn create_team_layout_in_caller_window(
    tmux_path: &str,
    caller_pane_id: &str,
    window_target: &str,
    members: &[TeamLayoutMember],
    server_url: &str,
    deps: &TeamLayoutDeps,
) -> Result<Option<CallerWindowLayout>, String> {
    let run = &deps.run_tmux_command;
    let mut panes_by_member = BTreeMap::new();
    let mut teammate_panes: Vec<String> = list_panes_in_window(tmux_path, window_target, deps)?
        .into_iter()
        .filter(|pane| pane != caller_pane_id)
        .collect();
    for member in members {
        let split = run(
            tmux_path,
            &build_split_args(caller_pane_id, &teammate_panes, member, deps),
        )?;
        if !split.success || split.output.is_empty() {
            return Ok(None);
        }
        let pane_id = split.output.trim().to_owned();
        teammate_panes.push(pane_id.clone());
        panes_by_member.insert(member.name.clone(), pane_id.clone());
        let title = format!("{TEAM_PANE_TITLE_PREFIX}{}", member.name);
        run(
            tmux_path,
            &strings(&["select-pane", "-t", &pane_id, "-T", &title]),
        )?;
        run(
            tmux_path,
            &strings(&[
                "set-option",
                "-p",
                "-t",
                &pane_id,
                OMO_ATTACH_SERVER_URL_OPTION,
                server_url,
            ]),
        )?;
        run(
            tmux_path,
            &strings(&[
                "set-option",
                "-p",
                "-t",
                &pane_id,
                OMO_ATTACH_SESSION_ID_OPTION,
                &member.session_id,
            ]),
        )?;
        let attach = build_attach_command(member, server_url, deps);
        run(
            tmux_path,
            &strings(&["send-keys", "-t", &pane_id, &attach, "Enter"]),
        )?;
    }
    if !run(
        tmux_path,
        &strings(&["select-layout", "-t", window_target, "main-vertical"]),
    )?
    .success
    {
        return Ok(None);
    }
    if !run(
        tmux_path,
        &strings(&["resize-pane", "-t", caller_pane_id, "-x", "30%"]),
    )?
    .success
    {
        return Ok(None);
    }
    Ok(Some((window_target.to_owned(), panes_by_member)))
}

/// `createTeamLayout`: split teammate panes into the caller's window and attach each session.
pub fn create_team_layout(
    team_run_id: &str,
    members: &[TeamLayoutMember],
    tmux_mgr: &dyn TmuxSessionManager,
    deps: &TeamLayoutDeps,
) -> Option<TeamLayoutResult> {
    if !can_visualize_with(&deps.env) {
        (deps.log)("tmux visualization unavailable, skipping", None);
        return None;
    }
    if members.is_empty() {
        return None;
    }
    let attempt = || -> Result<Option<TeamLayoutResult>, String> {
        let server_url = tmux_mgr.get_server_url();
        if !(deps.is_server_running)(&server_url) {
            let ctx_server_url = tmux_mgr.get_ctx_server_url().filter(|url| !url.is_empty());
            let discarded = ctx_server_url.as_ref().filter(|url| **url != server_url);
            let mut data =
                json!({ "kind": "warning", "teamRunId": team_run_id, "serverUrl": server_url });
            if let Some(url) = discarded {
                data["ctxServerUrl"] = json!(url);
            }
            data["hint"] = json!(if discarded.is_some() {
                "ctx.serverUrl was discarded (likely port 0); launch opencode with --port N and OPENCODE_PORT=N to bind a real port"
            } else {
                "no opencode server is listening on the fallback URL"
            });
            (deps.log)(
                "opencode server not reachable, skipping team layout (see issue #3963)",
                Some(data),
            );
            return Ok(None);
        }
        let Some(tmux_path) = (deps.get_tmux_path)()?.filter(|path| !path.is_empty()) else {
            (deps.log)("tmux visualization unavailable, skipping", None);
            return Ok(None);
        };
        let Some(caller) = (deps.resolve_caller_tmux_session)(&tmux_path) else {
            (deps.log)(
                "tmux visualization requires a resolvable caller tmux pane, skipping",
                Some(json!({ "teamRunId": team_run_id })),
            );
            return Ok(None);
        };
        let Some((focus_window_id, focus_panes_by_member)) = create_team_layout_in_caller_window(
            &tmux_path,
            &caller.pane_id,
            &caller.window_target,
            members,
            &server_url,
            deps,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(TeamLayoutResult {
            focus_window_id,
            grid_window_id: None,
            focus_panes_by_member,
            grid_panes_by_member: BTreeMap::new(),
            target_session_id: caller.session_id,
            owned_session: false,
        }))
    };
    attempt().unwrap_or_else(|error| {
        (deps.log)(
            "tmux visualization unavailable, skipping",
            Some(json!({ "error": error })),
        );
        None
    })
}

/// `removeTeamLayout`: kill the owned session, else the recorded panes, else the recorded windows.
pub fn remove_team_layout(
    team_run_id: &str,
    cleanup_target: Option<&TeamLayoutCleanupTarget>,
    deps: &TeamLayoutDeps,
) {
    if !can_visualize_with(&deps.env) {
        return;
    }
    let attempt = || -> Result<(), String> {
        let Some(tmux_path) = (deps.get_tmux_path)()?.filter(|path| !path.is_empty()) else {
            return Ok(());
        };
        let target = match cleanup_target {
            Some(target) if !target.owned_session => target,
            _ => {
                let session = cleanup_target.map_or_else(
                    || format!("omo-team-{team_run_id}"),
                    |target| target.target_session_id.clone(),
                );
                (deps.run_tmux_command)(&tmux_path, &strings(&["kill-session", "-t", &session]))?;
                return Ok(());
            }
        };
        if let Some(pane_ids) = target.pane_ids.as_ref().filter(|ids| !ids.is_empty()) {
            for pane_id in pane_ids {
                if (deps.run_tmux_command)(&tmux_path, &strings(&["kill-pane", "-t", pane_id]))
                    .is_err()
                {
                    (deps.log)(
                        "tmux team pane cleanup failed",
                        Some(json!({ "teamRunId": team_run_id, "paneId": pane_id })),
                    );
                }
            }
            return Ok(());
        }
        for window_id in [&target.focus_window_id, &target.grid_window_id]
            .into_iter()
            .flatten()
        {
            if window_id.is_empty() {
                continue;
            }
            if let Err(error) =
                (deps.run_tmux_command)(&tmux_path, &strings(&["kill-window", "-t", window_id]))
            {
                (deps.log)(
                    "tmux team layout window cleanup failed",
                    Some(
                        json!({ "teamRunId": team_run_id, "windowId": window_id, "error": error }),
                    ),
                );
            }
        }
        Ok(())
    };
    if let Err(error) = attempt() {
        (deps.log)(
            "tmux team layout cleanup failed",
            Some(json!({ "teamRunId": team_run_id, "error": error })),
        );
    }
}

// --- resolveCallerTmuxSession --------------------------------------------------------------

static TMUX_SESSION_ID_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$[0-9]+$").expect("valid regex"));
static TMUX_WINDOW_TARGET_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^:]+:[0-9]+$").expect("valid regex"));

/// `resolveCallerTmuxSession`: map the caller pane to its `$N` session id and `name:index` window.
pub fn resolve_caller_tmux_session(
    tmux_path: &str,
    caller_pane_id: Option<&str>,
    run_command: &dyn Fn(&str, &[String]) -> TmuxCommandResult,
) -> Option<ResolvedCallerTmuxSession> {
    let caller_pane_id = caller_pane_id.filter(|id| !id.is_empty())?;
    let session = run_command(
        tmux_path,
        &strings(&["display", "-p", "-F", "#{session_id}", "-t", caller_pane_id]),
    );
    if !session.success {
        return None;
    }
    let session_id = session.output.trim().to_owned();
    if !TMUX_SESSION_ID_PATTERN.is_match(&session_id) {
        return None;
    }
    let window = run_command(
        tmux_path,
        &strings(&[
            "display",
            "-p",
            "-F",
            "#{session_name}:#{window_index}",
            "-t",
            caller_pane_id,
        ]),
    );
    if !window.success {
        return None;
    }
    let window_target = window.output.trim().to_owned();
    if !TMUX_WINDOW_TARGET_PATTERN.is_match(&window_target) {
        return None;
    }
    Some(ResolvedCallerTmuxSession {
        session_id,
        pane_id: caller_pane_id.to_owned(),
        window_target,
    })
}

// --- closeTeamMemberPane -------------------------------------------------------------------

/// `CloseTeamMemberPaneDeps`; `Err` from `close_tmux_pane` mirrors a throw.
pub struct CloseTeamMemberPaneDeps<'a> {
    pub close_tmux_pane: &'a dyn Fn(&str) -> Result<bool, String>,
    pub log: &'a dyn Fn(&str, Option<Value>),
}

/// `closeTeamMemberPane` with the real tmux.
pub fn close_team_member_pane(member: &RuntimeStateMember) -> bool {
    close_team_member_pane_with(
        member.tmux_pane_id.as_deref(),
        member.tmux_grid_pane_id.as_deref(),
        &CloseTeamMemberPaneDeps {
            close_tmux_pane: &|pane| Ok(close_tmux_pane(pane)),
            log: &|m, d| logger::log(m, d),
        },
    )
}

/// `closeTeamMemberPane`: close the focus and grid panes; true when any close succeeded.
pub fn close_team_member_pane_with(
    tmux_pane_id: Option<&str>,
    tmux_grid_pane_id: Option<&str>,
    deps: &CloseTeamMemberPaneDeps<'_>,
) -> bool {
    let pane_ids: Vec<&str> = [tmux_pane_id, tmux_grid_pane_id]
        .into_iter()
        .flatten()
        .filter(|id| !id.is_empty())
        .collect();
    if pane_ids.is_empty() {
        return false;
    }
    let results: Vec<bool> = pane_ids
        .iter()
        .map(|pane_id| match (deps.close_tmux_pane)(pane_id) {
            Ok(closed) => closed,
            Err(error) => {
                (deps.log)(
                    "[closeTeamMemberPane] FAILED",
                    Some(json!({ "paneId": pane_id, "error": error })),
                );
                false
            }
        })
        .collect();
    results.into_iter().any(|closed| closed)
}

// --- rebalanceTeamWindow -------------------------------------------------------------------

/// `RebalanceLayout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebalanceLayout {
    MainVertical,
    Tiled,
}

impl RebalanceLayout {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MainVertical => "main-vertical",
            Self::Tiled => "tiled",
        }
    }
}

/// `RebalanceTeamWindowDeps`: `run_tmux` returns the command's success.
pub struct RebalanceTeamWindowDeps<'a> {
    pub run_tmux: &'a dyn Fn(&[String]) -> bool,
    pub log: &'a dyn Fn(&str, Option<Value>),
}

/// `rebalanceTeamWindowWith`.
pub fn rebalance_team_window_with(
    window_id: &str,
    layout: RebalanceLayout,
    deps: &RebalanceTeamWindowDeps<'_>,
) -> bool {
    if window_id.is_empty() {
        return false;
    }
    let fail = |step: &str| {
        (deps.log)(
            "[rebalanceTeamWindow] FAILED",
            Some(json!({ "windowId": window_id, "layout": layout.as_str(), "step": step })),
        );
        false
    };
    let select_layout = strings(&["select-layout", "-t", window_id, layout.as_str()]);
    if !(deps.run_tmux)(&select_layout) {
        return fail("select-layout");
    }
    if layout == RebalanceLayout::Tiled {
        return true;
    }
    if !(deps.run_tmux)(&strings(&[
        "set-window-option",
        "-t",
        window_id,
        "main-pane-width",
        "60%",
    ])) {
        return fail("set-window-option");
    }
    // tmux applies main-pane-width against the active layout, so select-layout again after resizing.
    if !(deps.run_tmux)(&select_layout) {
        return fail("select-layout");
    }
    true
}

/// `rebalanceTeamWindow` with the real tmux.
#[must_use]
pub fn rebalance_team_window(window_id: &str, layout: RebalanceLayout) -> bool {
    rebalance_team_window_with(
        window_id,
        layout,
        &RebalanceTeamWindowDeps {
            run_tmux: &|args| run_tmux_command("tmux", args, &RunTmuxOptions::default()).success,
            log: &|m, d| logger::log(m, d),
        },
    )
}

// --- sweepStaleTeamSessions ----------------------------------------------------------------

/// `TEAM_SESSION_PATTERN`: `omo-team-<uuid>`.
pub static TEAM_SESSION_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^omo-team-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$")
        .expect("valid regex")
});

/// `TeamSweepDeps`; `Err` mirrors a throw.
#[derive(Clone)]
pub struct TeamSweepDeps {
    pub list_candidates: ListCandidatesFn,
    pub kill_session: KillTeamSessionFn,
    pub log: LayoutLogFn,
}

/// `sweepStaleTeamSessionsWith`: kill `omo-team-<uuid>` sessions whose run is not active.
pub fn sweep_stale_team_sessions_with(
    active_team_run_ids: &HashSet<String>,
    deps: &TeamSweepDeps,
) -> Vec<String> {
    let list = Arc::clone(&deps.list_candidates);
    let kill = Arc::clone(&deps.kill_session);
    let log = Arc::clone(&deps.log);
    let list_candidate_sessions: SweepListFn = Arc::new(move |_tmux| list());
    let kill_session: SweepKillFn = Arc::new(move |name| kill(name).map(|()| true));
    let sweep_deps = SweepTmuxSessionsDeps {
        is_inside_tmux: Arc::new(|| true),
        get_tmux_path: Arc::new(|| Some("tmux".to_owned())),
        list_candidate_sessions,
        kill_session,
        log: Arc::new(move |message, data| log(message, data)),
    };
    let active = active_team_run_ids.clone();
    let options = SweepTmuxSessionsOptions {
        prefix: None,
        predicate: Some(Arc::new(move |session_name| {
            TEAM_SESSION_PATTERN
                .captures(session_name)
                .and_then(|captures| captures.get(1))
                .is_some_and(|id| !id.as_str().is_empty() && !active.contains(id.as_str()))
        })),
    };
    sweep_tmux_sessions_with(&sweep_deps, &options)
}

/// `sweepStaleTeamSessions` with the real tmux.
pub fn sweep_stale_team_sessions(active_team_run_ids: &HashSet<String>) -> Vec<String> {
    sweep_stale_team_sessions_with(
        active_team_run_ids,
        &TeamSweepDeps {
            list_candidates: Arc::new(|| {
                let result = run_tmux_command(
                    "tmux",
                    &strings(&["list-sessions", "-F", "#{session_name}"]),
                    &RunTmuxOptions::default(),
                );
                if !result.success {
                    return Ok(Vec::new());
                }
                Ok(result
                    .output
                    .split('\n')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect())
            }),
            kill_session: Arc::new(|name| {
                let result = run_tmux_command(
                    "tmux",
                    &strings(&["kill-session", "-t", name]),
                    &RunTmuxOptions::default(),
                );
                if result.success {
                    Ok(())
                } else {
                    Err(format!("Failed to kill tmux session: {name}"))
                }
            }),
            log: default_log(),
        },
    )
}
