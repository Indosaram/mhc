use crate::{HookEnvelope, extract_assistant_text, parse_endpoint, status_payload};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent, SessionReason};
use serde_json::{Map, Value, json};
use std::{sync::{Arc, Mutex, Condvar}, thread::JoinHandle, time::Duration};

#[derive(Default)]
struct Pending { value: Option<Post>, shutdown: bool }
struct Post { event: String, extra: Map<String, Value>, snapshot: Map<String, Value> }
#[derive(Default)]
struct EndpointCache {
    key: Option<(std::time::SystemTime, u64, u64)>,
    values: Option<std::collections::BTreeMap<String, String>>,
    warned: bool,
}
impl EndpointCache {
    fn read(&mut self, path: &std::path::Path) -> std::collections::BTreeMap<String, String> {
        let operation = || -> std::io::Result<_> {
            let metadata = path.metadata()?;
            #[cfg(unix)]
            let inode = { use std::os::unix::fs::MetadataExt; metadata.ino() };
            #[cfg(not(unix))]
            let inode = 0;
            let key = (metadata.modified()?, metadata.len(), inode);
            if self.key == Some(key) && let Some(values) = &self.values { return Ok((key, values.clone())); }
            Ok((key, parse_endpoint(&std::fs::read_to_string(path)?)))
        };
        match operation() {
            Ok((key, values)) => { self.key = Some(key); self.values = Some(values.clone()); values }
            Err(error) => {
                self.key = None;
                self.values = None;
                if error.kind() != std::io::ErrorKind::NotFound && !self.warned { self.warned = true; eprintln!("[orca-pi-status] failed to parse endpoint file: {error}"); }
                Default::default()
            }
        }
    }
}
struct Delivery { pending: Arc<(Mutex<Pending>, Condvar)>, thread: Mutex<Option<JoinHandle<()>>> }
fn windows_curl_path() -> Option<&'static str> {
    static PATH: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *PATH.get_or_init(|| {
        let wsl = std::env::var("WSL_DISTRO_NAME").is_ok_and(|value| !value.is_empty()) || ["/proc/sys/kernel/osrelease", "/proc/version"].iter().any(|path| std::fs::read_to_string(path).is_ok_and(|text| { let text = text.to_lowercase(); text.contains("microsoft") || text.contains("wsl") }));
        let path = "/mnt/c/Windows/System32/curl.exe";
        (wsl && std::path::Path::new(path).exists()).then_some(path)
    })
}
fn post_windows_curl(path: &str, url: &str, token: &str, body: &Value) {
    use std::io::Write;
    let child = std::process::Command::new(path).args(["-sS", "--connect-timeout", "3", "--max-time", "10", "--noproxy", "127.0.0.1", "-o", "NUL", "-X", "POST", "-H", "Content-Type: application/json", "-H", &format!("X-Orca-Agent-Hook-Token: {token}"), "--data-binary", "@-", url]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    if let Ok(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() { let _write = stdin.write_all(body.to_string().as_bytes()); }
        std::thread::spawn(move || { let _exit = child.wait(); });
    }
}
impl Delivery {
    fn new(metadata: Arc<Mutex<Map<String, Value>>>, omp: bool) -> Self {
        let pending = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let worker_pending = Arc::clone(&pending);
        let thread = std::thread::spawn(move || {
            let mut endpoints = EndpointCache::default();
            let client = match reqwest::blocking::Client::builder().timeout(Duration::from_millis(1000)).build() {
                Ok(client) => client,
                Err(error) => { eprintln!("Orca hook client: {error}"); return; }
            };
            loop {
                let (lock, signal) = &*worker_pending;
                let mut pending = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                while pending.value.is_none() && !pending.shutdown { pending = signal.wait(pending).unwrap_or_else(std::sync::PoisonError::into_inner); }
                let next = pending.value.take();
                if next.is_none() && pending.shutdown { break; }
                drop(pending);
                let Some(Post { event, extra, snapshot }) = next else { continue; };
                let file_env = std::env::var("ORCA_AGENT_HOOK_ENDPOINT").ok().filter(|path| !path.is_empty()).map(|path| endpoints.read(std::path::Path::new(&path))).unwrap_or_default();
                let lookup = |key: &str| file_env.get(key).filter(|value| !value.is_empty()).cloned().or_else(|| std::env::var(key).ok()).unwrap_or_default();
                let port = lookup("ORCA_AGENT_HOOK_PORT");
                let token = lookup("ORCA_AGENT_HOOK_TOKEN");
                let pane = std::env::var("ORCA_PANE_KEY").unwrap_or_default();
                if port.is_empty() || token.is_empty() || pane.is_empty() { continue; }
                let launch = std::env::var("ORCA_AGENT_LAUNCH_TOKEN").unwrap_or_default();
                let tab = std::env::var("ORCA_TAB_ID").unwrap_or_default();
                let worktree = std::env::var("ORCA_WORKTREE_ID").unwrap_or_default();
                let environment = lookup("ORCA_AGENT_HOOK_ENV");
                let version = lookup("ORCA_AGENT_HOOK_VERSION");
                let envelope = HookEnvelope { pane_key: &pane, launch_token: &launch, tab_id: &tab, worktree_id: &worktree, env: &environment, version: &version };
                let metadata = metadata.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let persisted = metadata.get("session_file").and_then(Value::as_str).is_some_and(|path| std::path::Path::new(path).exists());
                let empty = Map::new();
                let body = status_payload(&envelope, &event, if omp { &snapshot } else if persisted { &metadata } else { &empty }, &extra);
                drop(metadata);
                let route = if omp { "omp" } else { "pi" };
                let url = format!("http://127.0.0.1:{port}/hook/{route}");
                if client.post(&url).header("X-Orca-Agent-Hook-Token", &token).json(&body).send().is_err()
                    && let Some(path) = windows_curl_path() { post_windows_curl(path, &url, &token, &body); }
            }
        });
        Self { pending, thread: Mutex::new(Some(thread)) }
    }
    fn post(&self, event: &str, extra: Map<String, Value>, snapshot: Map<String, Value>) {
        let (lock, signal) = &*self.pending;
        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).value = Some(Post { event: event.to_owned(), extra, snapshot });
        signal.notify_one();
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        let (lock, signal) = &*self.pending;
        lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner).shutdown = true;
        signal.notify_one();
        if let Some(thread) = self.thread.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).take()
            && thread.join().is_err() { eprintln!("Orca hook thread panicked"); }
    }
}

fn update_metadata(metadata: &Mutex<Map<String, Value>>, ctx: &ExtensionContext) {
    let mut values = Map::new();
    let id = ctx.session_manager.session_id();
    if !id.is_empty() {
        values.insert("session_id".into(), json!(id));
        if let Some(path) = ctx.session_manager.session_file().filter(|path| !path.as_os_str().is_empty()) { values.insert("session_file".into(), json!(path.to_string_lossy())); }
    }
    *metadata.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = values;
}

pub struct OrcaAgentStatus;
impl Extension for OrcaAgentStatus {
    fn register(&self, api: &mut ExtensionApi) {
        let pid = std::process::id().to_string();
        if std::env::var("ORCA_PI_STATUS_OWNED").ok().is_some_and(|owner| !owner.is_empty() && owner != pid) { return; }
        let metadata = Arc::new(Mutex::new(Map::new()));
        let names = std::env::args().chain(std::env::var("_").ok()).collect::<Vec<_>>();
        let omp = crate::is_omp_runtime(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let delivery = Arc::new(Delivery::new(Arc::clone(&metadata), omp));
        let end_reported = Arc::new(Mutex::new(false));
        for kind in [EventKind::SessionStart, EventKind::BeforeAgentStart, EventKind::AgentStart, EventKind::ToolExecutionStart, EventKind::ToolCall, EventKind::ToolExecutionEnd, EventKind::MessageEnd, EventKind::AgentSettled] {
            let metadata = Arc::clone(&metadata);
            let delivery = Arc::clone(&delivery);
            let end_reported = Arc::clone(&end_reported);
            api.on(kind, Arc::new(move |event, ctx| {
                let metadata = Arc::clone(&metadata);
                let delivery = Arc::clone(&delivery);
                let end_reported = Arc::clone(&end_reported);
                Box::pin(async move {
                    let mut extra = Map::new();
                    let name = match event {
                        ExtensionEvent::SessionStart(start) => { update_metadata(&metadata, ctx); if start.reason == SessionReason::Reload { return Ok(EventResult::None); } "session_start" }
                        ExtensionEvent::BeforeAgentStart(start) => { extra.insert("prompt".into(), json!(start.prompt)); "before_agent_start" }
                        ExtensionEvent::AgentStart => { *end_reported.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = false; "agent_start" }
                        ExtensionEvent::ToolExecutionStart { tool_name, args, .. } => { extra.insert("tool_name".into(), json!(tool_name)); extra.insert("tool_input".into(), args.clone()); "tool_execution_start" }
                        ExtensionEvent::ToolCall(call) => { extra.insert("tool_name".into(), json!(call.tool_name)); extra.insert("tool_input".into(), call.input.clone()); "tool_call" }
                        ExtensionEvent::ToolExecutionEnd { tool_name, .. } => { extra.insert("tool_name".into(), json!(tool_name)); "tool_execution_end" }
                        ExtensionEvent::MessageEnd { message } => {
                            let value = serde_json::to_value(message).map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
                            if value.get("role").and_then(Value::as_str) != Some("assistant") { return Ok(EventResult::None); }
                            let text = extract_assistant_text(&value);
                            if text.is_empty() { return Ok(EventResult::None); }
                            extra.insert("role".into(), json!("assistant")); extra.insert("text".into(), json!(text)); "message_end"
                        }
                        ExtensionEvent::AgentSettled => { let mut reported = end_reported.lock().unwrap_or_else(std::sync::PoisonError::into_inner); if *reported { return Ok(EventResult::None); } *reported = true; "agent_end" }
                        _ => return Ok(EventResult::None),
                    };
                    let mut snapshot = Map::new();
                    if name != "session_start" && omp {
                        let id = ctx.session_manager.session_id();
                        if !id.is_empty() && ctx.session_manager.session_file().is_some_and(|path| !path.as_os_str().is_empty()) { snapshot.insert("session_id".into(), json!(id)); }
                    }
                    delivery.post(name, extra, snapshot);
                    Ok(EventResult::None)
                })
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoint_cache_refreshes_after_size_change() {
        let fixture = tempfile::tempdir().expect("create endpoint fixture");
        let path = fixture.path().join("endpoint.env");
        std::fs::write(&path, "ORCA_AGENT_HOOK_PORT=123").expect("write endpoint");
        let mut cache = EndpointCache::default();
        assert_eq!(cache.read(&path).get("ORCA_AGENT_HOOK_PORT").map(String::as_str), Some("123"));
        let initial = cache.key;
        cache.read(&path);
        assert_eq!(cache.key, initial);
        std::fs::write(&path, "ORCA_AGENT_HOOK_PORT=12345").expect("change endpoint size");
        assert_eq!(cache.read(&path).get("ORCA_AGENT_HOOK_PORT").map(String::as_str), Some("12345"));
    }
    #[test]
    fn missing_endpoint_clears_cache_without_warning() {
        let fixture = tempfile::tempdir().expect("create missing endpoint fixture");
        let mut cache = EndpointCache::default();
        assert!(cache.read(&fixture.path().join("missing")).is_empty());
        assert!(!cache.warned);
        assert!(cache.values.is_none());
    }
}
