//! Injectable host for the acceptance tests (spec §8).

#![allow(dead_code)]

use maho_git_bash::{Host, RunOutput, RunRequest, Server, ServerOptions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

pub const GIT_BASH: &str = r"C:\Program Files\Git\bin\bash.exe";

pub struct FakeHost {
    pub platform: String,
    pub env: BTreeMap<String, String>,
    pub files: BTreeSet<String>,
    pub where_output: String,
    pub run_result: Result<RunOutput, String>,
    pub requests: Mutex<Vec<RunRequest>>,
}

impl FakeHost {
    pub fn new(platform: &str) -> Self {
        Self {
            platform: platform.to_owned(),
            env: BTreeMap::new(),
            files: BTreeSet::new(),
            where_output: String::new(),
            run_result: Ok(RunOutput {
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
            }),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Windows with Git Bash under Program Files.
    pub fn windows_with_git_bash() -> Self {
        Self::new("win32").with_file(GIT_BASH)
    }

    pub fn with_env(mut self, name: &str, value: &str) -> Self {
        self.env.insert(name.to_owned(), value.to_owned());
        self
    }

    pub fn with_file(mut self, path: &str) -> Self {
        self.files.insert(path.to_owned());
        self
    }

    pub fn with_where(mut self, output: &str) -> Self {
        self.where_output = output.to_owned();
        self
    }

    pub fn with_run_result(mut self, result: Result<RunOutput, String>) -> Self {
        self.run_result = result;
        self
    }

    pub fn recorded(&self) -> Vec<RunRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl Host for FakeHost {
    fn platform(&self) -> String {
        self.platform.clone()
    }

    fn env_var(&self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn file_exists(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    fn where_bash(&self) -> String {
        self.where_output.clone()
    }

    async fn run(&self, request: RunRequest) -> Result<RunOutput, String> {
        self.requests.lock().expect("requests lock").push(request);
        self.run_result.clone()
    }
}

pub fn server(host: FakeHost) -> Server<FakeHost> {
    Server::new(host, ServerOptions::default())
}

pub fn tool_names(server: &Server<FakeHost>) -> Vec<String> {
    server
        .tool_definitions()
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name").to_owned())
        .collect()
}

pub fn parse(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("tool reply text is JSON")
}
