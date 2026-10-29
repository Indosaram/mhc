use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::fake_session::FakeSession;
use crate::host::HostError;
use crate::runners::in_process::child_handle::ChildSession;
use crate::runners::in_process::child_options::ChildSessionOptions;
use crate::runners::in_process::runner::{
    ChildSpec, CreateChildSession, InProcessRunner, InProcessRunnerOptions,
};
use crate::runners::in_process::shared_tool_filter::{ChildTool, ChildToolRef};

pub(super) struct FakeTool {
    name: String,
    description: String,
    ran: Arc<AtomicBool>,
}

impl ChildTool for FakeTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn execute(&self, _tool_call_id: &str, _input: &Value) -> Result<Value, HostError> {
        self.ran.store(true, Ordering::SeqCst);
        Ok(json!({ "content": [{ "type": "text", "text": "ok" }] }))
    }
}

pub(super) fn make_tool(name: &str) -> ChildToolRef {
    make_tracked_tool(name, &Arc::default())
}

pub(super) fn make_tracked_tool(name: &str, ran: &Arc<AtomicBool>) -> ChildToolRef {
    Arc::new(FakeTool {
        name: name.to_string(),
        description: format!("test tool {name}"),
        ran: Arc::clone(ran),
    })
}

pub(super) fn tool_names(options: &ChildSessionOptions) -> Vec<String> {
    options
        .custom_tools
        .iter()
        .map(|tool| tool.name().to_string())
        .collect()
}

pub(super) type Captured = Arc<Mutex<Vec<ChildSessionOptions>>>;

/// A runner whose host `createSession` records every options object and returns `session`.
pub(super) fn capturing_runner(
    shared_parent_tools: Vec<ChildToolRef>,
    ui_only_tool_names: &[&str],
    max_depth: Option<u32>,
    session: impl Fn() -> Arc<dyn ChildSession> + Send + Sync + 'static,
) -> (InProcessRunner, Captured) {
    let captured: Captured = Arc::default();
    let sink = Arc::clone(&captured);
    let create: CreateChildSession = Arc::new(move |options| {
        sink.lock().expect("captured").push(options);
        Ok(session())
    });
    let runner = InProcessRunner::new(InProcessRunnerOptions {
        shared_parent_tools,
        ui_only_tool_names: ui_only_tool_names
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        max_depth,
        create_session: create,
    });
    (runner, captured)
}

pub(super) fn fake_runner(fake: &Arc<FakeSession>) -> (InProcessRunner, Captured) {
    let fake = Arc::clone(fake);
    capturing_runner(Vec::new(), &[], None, move || fake.clone())
}

pub(super) fn last_options(captured: &Captured) -> ChildSessionOptions {
    captured
        .lock()
        .expect("captured")
        .last()
        .cloned()
        .expect("createSession was called")
}

pub(super) fn base_spec(session_dir: &str) -> ChildSpec {
    ChildSpec {
        task_id: "task-1".to_string(),
        cwd: std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .into_owned(),
        session_dir: Some(session_dir.to_string()),
        depth: 0,
        parent_session_id: "parent-1".to_string(),
        root_session_id: "root-1".to_string(),
        prompt: "do the work".to_string(),
        ..ChildSpec::default()
    }
}

pub(super) fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

pub(super) fn session_dir_in(dir: &tempfile::TempDir, task_id: &str) -> String {
    dir.path()
        .join("sessions")
        .join(task_id)
        .to_string_lossy()
        .into_owned()
}

pub(super) fn write_session_file(dir: &tempfile::TempDir, content: &str) -> std::path::PathBuf {
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, content).expect("write session file");
    path
}

pub(super) fn session_header(id: &str) -> String {
    format!(
        "{}\n",
        json!({ "type": "session", "id": id, "timestamp": "2026-08-04T00:00:00.000Z", "cwd": "/tmp" })
    )
}

pub(super) fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
