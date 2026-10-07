use std::path::Path;
use std::sync::{Arc, Mutex};

use isolation_core::{BackendRuntime, CommandResult, IsolationResult};

#[derive(Clone)]
pub struct FakeRuntime {
    pub platform: String,
    pub which: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub run: Arc<dyn Fn(&[String]) -> IsolationResult<CommandResult> + Send + Sync>,
    pub device: Arc<dyn Fn(&Path) -> IsolationResult<u64> + Send + Sync>,
    pub accessible: Arc<dyn Fn(&Path) -> IsolationResult<bool> + Send + Sync>,
    pub mounted: Arc<dyn Fn(&Path) -> IsolationResult<bool> + Send + Sync>,
    pub wait_mounted: Arc<dyn Fn(&Path) -> IsolationResult<()> + Send + Sync>,
}

impl FakeRuntime {
    pub fn linux() -> Self {
        FakeRuntime {
            platform: "linux".to_string(),
            which: Arc::new(|_| true),
            run: Arc::new(|_| {
                Ok(CommandResult {
                    code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }),
            device: Arc::new(|_| Ok(1)),
            accessible: Arc::new(|_| Ok(true)),
            mounted: Arc::new(|_| Ok(false)),
            wait_mounted: Arc::new(|_| Ok(())),
        }
    }
}

impl BackendRuntime for FakeRuntime {
    fn platform(&self) -> &str {
        &self.platform
    }

    fn which(&self, binary: &str) -> bool {
        (self.which)(binary)
    }

    fn run(&self, argv: &[String]) -> IsolationResult<CommandResult> {
        (self.run)(argv)
    }

    fn device(&self, path: &Path) -> IsolationResult<u64> {
        (self.device)(path)
    }

    fn accessible(&self, path: &Path) -> IsolationResult<bool> {
        (self.accessible)(path)
    }

    fn mounted(&self, path: &Path) -> IsolationResult<bool> {
        (self.mounted)(path)
    }

    fn wait_mounted(&self, path: &Path) -> IsolationResult<()> {
        (self.wait_mounted)(path)
    }
}

pub type Calls = Arc<Mutex<Vec<Vec<String>>>>;

pub fn calls() -> Calls {
    Arc::new(Mutex::new(Vec::new()))
}

pub fn record(calls: &Calls, argv: &[String]) {
    calls
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(argv.to_vec());
}

pub fn recorded(calls: &Calls) -> Vec<Vec<String>> {
    calls
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

pub fn ok() -> CommandResult {
    CommandResult {
        code: 0,
        stdout: String::new(),
        stderr: String::new(),
    }
}

pub fn result(code: i32, stdout: &str, stderr: &str) -> CommandResult {
    CommandResult {
        code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    }
}
