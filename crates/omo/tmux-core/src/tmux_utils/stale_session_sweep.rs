//! Sweep tmux sessions: generic predicate/prefix sweep and stale `omo-agents-<pid>` cleanup.

use std::sync::Arc;

use serde_json::json;

use crate::runner::{RunTmuxOptions, run_tmux_command};
use crate::tmux_utils::deps::{GetTmuxPathFn, LogFn, PredicateFn, TmuxDeps, strings};
use crate::tmux_utils::session_kill::kill_tmux_session_if_exists;

pub type ListCandidateSessionsFn = Arc<dyn Fn(&str) -> Result<Vec<String>, String> + Send + Sync>;
pub type KillSessionFn = Arc<dyn Fn(&str) -> Result<bool, String> + Send + Sync>;
pub type ProcessAliveFn = Arc<dyn Fn(u32) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct SweepTmuxSessionsDeps {
    pub is_inside_tmux: PredicateFn,
    pub get_tmux_path: GetTmuxPathFn,
    /// `Err` mirrors a thrown listing error: logged, sweep returns nothing.
    pub list_candidate_sessions: ListCandidateSessionsFn,
    /// `Err` mirrors a thrown kill error: logged, sweep continues.
    pub kill_session: KillSessionFn,
    pub log: LogFn,
}

#[derive(Clone)]
pub struct SweepDeps {
    pub sweep: SweepTmuxSessionsDeps,
    pub process_alive: ProcessAliveFn,
    pub current_pid: u32,
}

pub type SessionPredicateFn = Arc<dyn Fn(&str) -> bool + Send + Sync>;

#[derive(Clone, Default)]
pub struct SweepTmuxSessionsOptions {
    pub prefix: Option<String>,
    /// Takes precedence over [`Self::prefix`].
    pub predicate: Option<SessionPredicateFn>,
}

fn matches_sweep_options(session_name: &str, options: &SweepTmuxSessionsOptions) -> bool {
    if let Some(predicate) = &options.predicate {
        return predicate(session_name);
    }
    match options
        .prefix
        .as_deref()
        .filter(|prefix| !prefix.is_empty())
    {
        Some(prefix) => session_name.starts_with(prefix),
        None => true,
    }
}

/// Kill every candidate session matching `options`; returns the killed names in order.
pub fn sweep_tmux_sessions_with(
    deps: &SweepTmuxSessionsDeps,
    options: &SweepTmuxSessionsOptions,
) -> Vec<String> {
    if !(deps.is_inside_tmux)() {
        return Vec::new();
    }
    let Some(tmux) = (deps.get_tmux_path)() else {
        return Vec::new();
    };

    let candidates = match (deps.list_candidate_sessions)(&tmux) {
        Ok(candidates) => candidates,
        Err(error) => {
            (deps.log)(
                "[sweepTmuxSessionsWith] failed to list candidate sessions",
                Some(json!({ "error": error })),
            );
            return Vec::new();
        }
    };

    let mut killed = Vec::new();
    for session_name in candidates {
        if !matches_sweep_options(&session_name, options) {
            continue;
        }
        match (deps.kill_session)(&session_name) {
            Ok(true) => killed.push(session_name),
            Ok(false) => {}
            Err(error) => (deps.log)(
                "[sweepTmuxSessionsWith] failed to kill stale session",
                Some(json!({ "error": error, "sessionName": session_name })),
            ),
        }
    }
    killed
}

/// Parse `omo-agents-<pid>` / `omo-agents-<pid>-<alnum>` into the owner pid.
fn stale_session_pid(session_name: &str) -> Option<u32> {
    let rest = session_name.strip_prefix("omo-agents-")?;
    let (pid, suffix) = rest.split_once('-').unwrap_or((rest, ""));
    let digits_only = !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit());
    let suffix_valid = rest.len() == pid.len()
        || (!suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    if !digits_only || !suffix_valid {
        return None;
    }
    pid.parse().ok()
}

/// Kill `omo-agents-<pid>` sessions whose owner process is dead (never the current pid).
pub fn sweep_stale_omo_agent_sessions_with(deps: &SweepDeps) -> usize {
    let process_alive = Arc::clone(&deps.process_alive);
    let current_pid = deps.current_pid;
    let options = SweepTmuxSessionsOptions {
        prefix: None,
        predicate: Some(Arc::new(move |session_name| {
            stale_session_pid(session_name)
                .is_some_and(|pid| pid != current_pid && !process_alive(pid))
        })),
    };
    sweep_tmux_sessions_with(&deps.sweep, &options).len()
}

fn list_tmux_sessions_via_tmux(tmux: &str) -> Result<Vec<String>, String> {
    let result = run_tmux_command(
        tmux,
        &strings(&["list-sessions", "-F", "#{session_name}"]),
        &RunTmuxOptions::default(),
    );
    if result.exit_code != 0 {
        return Ok(Vec::new());
    }
    Ok(result
        .output
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect())
}

/// [`sweep_stale_omo_agent_sessions_with`] with runtime defaults.
#[must_use]
pub fn sweep_stale_omo_agent_sessions() -> usize {
    let defaults = TmuxDeps::default();
    let inside = defaults.clone();
    sweep_stale_omo_agent_sessions_with(&SweepDeps {
        sweep: SweepTmuxSessionsDeps {
            is_inside_tmux: Arc::new(move || inside.inside_tmux()),
            get_tmux_path: Arc::clone(&defaults.get_tmux_path),
            list_candidate_sessions: Arc::new(list_tmux_sessions_via_tmux),
            kill_session: Arc::new(move |name| Ok(kill_tmux_session_if_exists(name, &defaults))),
            log: Arc::new(|_, _| {}),
        },
        process_alive: Arc::new(utils::process_sweep::default_is_process_alive),
        current_pid: std::process::id(),
    })
}
