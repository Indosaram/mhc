//! Port of src/tmux-utils/stale-session-sweep.test.ts

use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use tmux_core::{
    SweepDeps, SweepTmuxSessionsDeps, SweepTmuxSessionsOptions,
    sweep_stale_omo_agent_sessions_with, sweep_tmux_sessions_with,
};

const CURRENT_PID: u32 = 12345;

type AliveFn = Arc<dyn Fn(u32) -> bool + Send + Sync>;

struct Fixture {
    candidates: Arc<Mutex<Vec<String>>>,
    killed: Arc<Mutex<Vec<String>>>,
    kill_calls: Arc<Mutex<usize>>,
    kill_result: Arc<Mutex<bool>>,
    alive: Arc<Mutex<AliveFn>>,
    inside_tmux: bool,
    tmux_path: Option<String>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            candidates: Arc::default(),
            killed: Arc::default(),
            kill_calls: Arc::default(),
            kill_result: Arc::new(Mutex::new(true)),
            alive: Arc::new(Mutex::new(Arc::new(|_| false))),
            inside_tmux: true,
            tmux_path: Some("tmux".into()),
        }
    }

    fn set_candidates(&self, sessions: &[&str]) {
        *self.candidates.lock().unwrap() = sessions.iter().map(|s| (*s).to_owned()).collect();
    }

    fn set_alive(&self, predicate: impl Fn(u32) -> bool + Send + Sync + 'static) {
        *self.alive.lock().unwrap() = Arc::new(predicate);
    }

    fn killed(&self) -> Vec<String> {
        self.killed.lock().unwrap().clone()
    }

    fn sweep_deps(&self) -> SweepTmuxSessionsDeps {
        let inside_tmux = self.inside_tmux;
        let tmux_path = self.tmux_path.clone();
        let candidates = Arc::clone(&self.candidates);
        let (killed, kill_calls, kill_result) = (
            Arc::clone(&self.killed),
            Arc::clone(&self.kill_calls),
            Arc::clone(&self.kill_result),
        );
        SweepTmuxSessionsDeps {
            is_inside_tmux: Arc::new(move || inside_tmux),
            get_tmux_path: Arc::new(move || tmux_path.clone()),
            list_candidate_sessions: Arc::new(move |_| Ok(candidates.lock().unwrap().clone())),
            kill_session: Arc::new(move |name| {
                *kill_calls.lock().unwrap() += 1;
                let result = *kill_result.lock().unwrap();
                if result {
                    killed.lock().unwrap().push(name.to_owned());
                }
                Ok(result)
            }),
            log: Arc::new(|_, _| {}),
        }
    }

    fn deps(&self) -> SweepDeps {
        let alive = Arc::clone(&self.alive);
        SweepDeps {
            sweep: self.sweep_deps(),
            process_alive: Arc::new(move |pid| (alive.lock().unwrap())(pid)),
            current_pid: CURRENT_PID,
        }
    }
}

// describe("sweepStaleOmoAgentSessionsWith")

#[test]
fn not_inside_tmux_returns_zero_without_listing() {
    let fixture = Fixture {
        inside_tmux: false,
        ..Fixture::new()
    };
    fixture.set_candidates(&["omo-agents-99991"]);
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 0);
    assert!(fixture.killed().is_empty());
}

#[test]
fn tmux_not_found_returns_zero_without_listing() {
    let fixture = Fixture {
        tmux_path: None,
        ..Fixture::new()
    };
    fixture.set_candidates(&["omo-agents-99991"]);
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 0);
    assert!(fixture.killed().is_empty());
}

#[test]
fn empty_candidate_list_returns_zero_and_kills_nothing() {
    let fixture = Fixture::new();
    fixture.set_candidates(&[]);
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 0);
    assert!(fixture.killed().is_empty());
}

#[test]
fn sessions_with_dead_pids_are_each_killed_once() {
    let fixture = Fixture::new();
    fixture.set_candidates(&["omo-agents-99991", "omo-agents-99992"]);
    fixture.set_alive(|_| false);
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 2);
    assert_eq!(
        fixture.killed(),
        vec!["omo-agents-99991", "omo-agents-99992"]
    );
}

#[test]
fn suffixed_sessions_with_dead_pids_are_also_killed() {
    let fixture = Fixture::new();
    fixture.set_candidates(&["omo-agents-99991-1", "omo-agents-99992-abc123"]);
    fixture.set_alive(|_| false);
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 2);
    assert_eq!(
        fixture.killed(),
        vec!["omo-agents-99991-1", "omo-agents-99992-abc123"]
    );
}

#[test]
fn session_matching_current_pid_is_not_killed() {
    let fixture = Fixture::new();
    let own = format!("omo-agents-{CURRENT_PID}");
    fixture.set_candidates(&[&own, "omo-agents-99999"]);
    fixture.set_alive(|_| false);
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 1);
    assert_eq!(fixture.killed(), vec!["omo-agents-99999"]);
}

#[test]
fn session_with_live_pid_is_not_killed() {
    let fixture = Fixture::new();
    fixture.set_candidates(&["omo-agents-88888"]);
    fixture.set_alive(|pid| pid == 88888);
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 0);
    assert!(fixture.killed().is_empty());
}

#[test]
fn kill_session_false_is_not_counted() {
    let fixture = Fixture::new();
    fixture.set_candidates(&["omo-agents-55555"]);
    fixture.set_alive(|_| false);
    *fixture.kill_result.lock().unwrap() = false;
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 0);
    assert_eq!(*fixture.kill_calls.lock().unwrap(), 1);
}

#[test]
fn only_supported_omo_agents_names_are_considered() {
    let fixture = Fixture::new();
    fixture.set_candidates(&[
        "main",
        "omo-agents-99999",
        "omo-agents-99999-1-2",
        "other-session",
        "omo-agents-abc",
    ]);
    fixture.set_alive(|_| false);
    assert_eq!(sweep_stale_omo_agent_sessions_with(&fixture.deps()), 1);
    assert_eq!(fixture.killed(), vec!["omo-agents-99999"]);
}

// describe("sweepTmuxSessionsWith")

#[test]
fn custom_predicate_kills_only_matching_team_sessions() {
    let fixture = Fixture::new();
    fixture.set_candidates(&["omo-team-A", "omo-team-B", "main", "omo-agents-99999"]);
    let options = SweepTmuxSessionsOptions {
        prefix: None,
        predicate: Some(Arc::new(|name: &str| name.starts_with("omo-team-"))),
    };
    let result = sweep_tmux_sessions_with(&fixture.sweep_deps(), &options);
    assert_eq!(result, vec!["omo-team-A", "omo-team-B"]);
    assert_eq!(fixture.killed(), vec!["omo-team-A", "omo-team-B"]);
}
