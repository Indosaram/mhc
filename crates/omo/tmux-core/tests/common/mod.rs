#![allow(
    dead_code,
    reason = "each test binary uses a different subset of helpers"
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tmux_core::{EnvSource, TmuxCommandResult, TmuxConfig, TmuxDeps, TmuxIsolation, TmuxLayout};

pub type Call = (String, Vec<String>);

pub fn enabled_config() -> TmuxConfig {
    TmuxConfig {
        enabled: true,
        layout: TmuxLayout::MainVertical,
        main_pane_size: 60,
        main_pane_min_width: 120,
        agent_pane_min_width: 40,
        isolation: TmuxIsolation::Inline,
    }
}

pub fn env(pairs: &[(&str, &str)]) -> Arc<dyn EnvSource> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    Arc::new(map)
}

pub fn success(output: &str) -> TmuxCommandResult {
    TmuxCommandResult::new(output, "", 0)
}

pub fn failed() -> TmuxCommandResult {
    TmuxCommandResult::new("", "", 1)
}

/// Records every runner call and replays a fixed result queue (panics when exhausted).
#[derive(Clone, Default)]
pub struct Recorder {
    pub calls: Arc<Mutex<Vec<Call>>>,
    results: Arc<Mutex<Vec<TmuxCommandResult>>>,
}

impl Recorder {
    pub fn new(results: Vec<TmuxCommandResult>) -> Self {
        Self {
            calls: Arc::default(),
            results: Arc::new(Mutex::new(results)),
        }
    }

    pub fn runner(&self) -> tmux_core::RunTmuxCommandFn {
        let calls = Arc::clone(&self.calls);
        let results = Arc::clone(&self.results);
        Arc::new(move |tmux, args| {
            calls.lock().unwrap().push((tmux.to_owned(), args.to_vec()));
            let mut results = results.lock().unwrap();
            assert!(
                !results.is_empty(),
                "No more tmux command results configured"
            );
            results.remove(0)
        })
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn call(&self, index: usize) -> Call {
        self.calls()
            .get(index)
            .cloned()
            .unwrap_or_else(|| panic!("Expected tmux runner call at index {index}"))
    }

    /// The last argument (the shell command) of call `index`.
    pub fn command(&self, index: usize) -> String {
        self.call(index)
            .1
            .last()
            .cloned()
            .expect("Expected a command argument")
    }
}

/// Healthy deps: inside tmux, server running, tmux at `tmux_path`, empty environment.
pub fn healthy_deps(recorder: &Recorder, tmux_path: &str) -> TmuxDeps {
    let tmux_path = tmux_path.to_owned();
    TmuxDeps {
        run_tmux_command: recorder.runner(),
        is_inside_tmux: Some(Arc::new(|| true)),
        is_server_running: Arc::new(|_| true),
        get_tmux_path: Arc::new(move || Some(tmux_path.clone())),
        environment: env(&[]),
        ..TmuxDeps::default()
    }
}

pub fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}
