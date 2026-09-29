//! Scripted LSP server used by the lsp-core integration tests (port of
//! `fixtures/workspace-edit-server.mjs`). Usage: `<bin> <scenario.json> <events.jsonl>`.

use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::time::Duration;

struct RenameContext {
    rename_request_id: Value,
    step: Value,
    apply_count: u32,
    pending_apply_responses: Option<u32>,
}

enum PendingRequest {
    PublishDeliveryBarrier,
    Unscoped,
    Rename(Arc<Mutex<RenameContext>>),
}

struct Server {
    scenario: Value,
    events_path: String,
    stdout: Mutex<std::io::Stdout>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next_server_request_id: u64,
    rename_index: usize,
    diagnostic_index: usize,
    pending: HashMap<String, PendingRequest>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn id_key(id: &Value) -> String {
    match id {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn delay_ms(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_f64)
        .map_or(0, |ms| ms.max(0.0) as u64)
}

impl Server {
    fn record(&self, event: Value) {
        let _guard = lock(&self.stdout);
        if let Ok(mut file) = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.events_path)
        {
            let _ = writeln!(file, "{event}");
        }
    }

    fn send(&self, message: Value) {
        let body = message.to_string();
        let mut stdout = lock(&self.stdout);
        let _ = write!(stdout, "Content-Length: {}\r\n\r\n{body}", body.len());
        let _ = stdout.flush();
    }

    fn send_response(&self, id: &Value, result: Value) {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn later(self: &Arc<Self>, delay: u64, action: impl FnOnce(&Arc<Self>) + Send + 'static) {
        if delay == 0 {
            action(self);
            return;
        }
        let server = Arc::clone(self);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay));
            action(&server);
        });
    }

    fn next_request_id(&self, pending: PendingRequest) -> u64 {
        let mut state = lock(&self.state);
        if state.next_server_request_id == 0 {
            state.next_server_request_id = 10_000;
        }
        let id = state.next_server_request_id;
        state.next_server_request_id += 1;
        state.pending.insert(id.to_string(), pending);
        id
    }

    fn send_apply_edit(&self, edit: &Value, pending: PendingRequest) {
        let id = self.next_request_id(pending);
        self.record(json!({ "type": "serverRequest", "method": "workspace/applyEdit", "id": id, "edit": edit }));
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "workspace/applyEdit", "params": { "edit": edit } }));
    }

    fn finish_rename(self: &Arc<Self>, rename_request_id: Value, step: &Value) {
        let result = match step.get("renameResult").and_then(Value::as_str) {
            Some("same") => step.get("applyEdit").cloned().unwrap_or(Value::Null),
            Some("different") => step.get("differentEdit").cloned().unwrap_or(Value::Null),
            Some("edit") => step.get("renameEdit").cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
        self.later(delay_ms(step.get("responseDelayMs")), move |server| {
            server.record(json!({ "type": "serverResponse", "method": "textDocument/rename", "result": result }));
            server.send_response(&rename_request_id, result);
        });
    }

    fn send_publish_delivery_barrier(&self) {
        let id = self.next_request_id(PendingRequest::PublishDeliveryBarrier);
        self.record(
            json!({ "type": "serverRequest", "method": "workspace/configuration", "id": id }),
        );
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "workspace/configuration", "params": { "items": [] } }));
    }

    fn apply_publish_steps(self: &Arc<Self>, trigger: &str, fallback_uri: Option<Value>) {
        let steps: Vec<Value> = self
            .scenario
            .get("publishDiagnostics")
            .and_then(Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .filter(|step| step.get("trigger").and_then(Value::as_str) == Some(trigger))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        for step in steps {
            let fallback = fallback_uri.clone();
            self.later(delay_ms(step.get("delayMs")), move |server| {
                let mut params = serde_json::Map::new();
                params.insert(
                    "uri".into(),
                    step.get("uri").cloned().or(fallback).unwrap_or(Value::Null),
                );
                if let Some(version) = step.get("version") {
                    params.insert("version".into(), version.clone());
                }
                let diagnostics = step
                    .get("diagnostics")
                    .filter(|value| value.is_array())
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                params.insert("diagnostics".into(), diagnostics);
                let params = Value::Object(params);
                server.record(json!({
                    "type": "serverNotification", "method": "textDocument/publishDiagnostics", "params": params,
                }));
                server.send(json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": params }));
                if step.get("awaitClientDelivery") == Some(&Value::Bool(true)) {
                    server.send_publish_delivery_barrier();
                }
            });
        }
    }

    fn handle_client_response(self: &Arc<Self>, message: &Value) {
        let Some(pending) = lock(&self.state).pending.remove(&id_key(&message["id"])) else {
            return;
        };
        let method = match pending {
            PendingRequest::PublishDeliveryBarrier => "workspace/configuration",
            PendingRequest::Unscoped | PendingRequest::Rename(_) => "workspace/applyEdit",
        };
        self.record(json!({
            "type": "clientResponse",
            "method": method,
            "result": message.get("result").cloned().unwrap_or(Value::Null),
            "error": message.get("error").cloned().unwrap_or(Value::Null),
        }));
        let PendingRequest::Rename(context) = pending else {
            return;
        };
        let mut guard = lock(&context);
        if let Some(remaining) = guard.pending_apply_responses.as_mut() {
            *remaining -= 1;
            if *remaining > 0 {
                return;
            }
        } else if guard.step.get("applyTwice") == Some(&Value::Bool(true)) && guard.apply_count == 1
        {
            guard.apply_count = 2;
            let edit = guard.step["applyEdit"].clone();
            drop(guard);
            self.send_apply_edit(&edit, PendingRequest::Rename(Arc::clone(&context)));
            return;
        }
        let (id, step) = (guard.rename_request_id.clone(), guard.step.clone());
        drop(guard);
        self.finish_rename(id, &step);
    }

    fn handle_request(self: &Arc<Self>, message: &Value) {
        let id = message["id"].clone();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match method {
            "initialize" => {
                self.record(json!({
                    "type": "clientRequest", "method": method, "id": id, "params": message.get("params"),
                }));
                let capabilities = self
                    .scenario
                    .get("capabilities")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                self.send_response(&id, json!({ "capabilities": capabilities }));
            }
            "textDocument/rename" => {
                let (step, rename_index) = {
                    let mut state = lock(&self.state);
                    let step = self.scenario["renameSteps"]
                        .get(state.rename_index)
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    state.rename_index += 1;
                    (step, state.rename_index)
                };
                self.record(json!({
                    "type": "clientRequest", "method": method, "id": id,
                    "params": message.get("params"), "renameIndex": rename_index,
                }));
                let Some(edit) = step
                    .get("applyEdit")
                    .filter(|edit| !edit.is_null())
                    .cloned()
                else {
                    self.finish_rename(id, &step);
                    return;
                };
                let concurrent = step.get("applyConcurrently") == Some(&Value::Bool(true));
                let delay = delay_ms(step.get("applyDelayMs"));
                let context = Arc::new(Mutex::new(RenameContext {
                    rename_request_id: id,
                    step,
                    apply_count: 1,
                    pending_apply_responses: concurrent.then_some(2),
                }));
                if concurrent {
                    self.send_apply_edit(&edit, PendingRequest::Rename(Arc::clone(&context)));
                    self.send_apply_edit(&edit, PendingRequest::Rename(context));
                    return;
                }
                self.later(delay, move |server| {
                    server.send_apply_edit(&edit, PendingRequest::Rename(context));
                });
            }
            "textDocument/diagnostic" => {
                let (step, diagnostic_index) = {
                    let mut state = lock(&self.state);
                    let step = self.scenario["diagnosticResponses"]
                        .get(state.diagnostic_index)
                        .cloned();
                    state.diagnostic_index += 1;
                    (step, state.diagnostic_index)
                };
                self.record(json!({
                    "type": "clientRequest", "method": method, "id": id,
                    "params": message.get("params"), "diagnosticIndex": diagnostic_index,
                }));
                let uri = message.pointer("/params/textDocument/uri").cloned();
                self.apply_publish_steps("diagnosticRequest", uri);
                let step = step.unwrap_or(Value::Null);
                let delay = delay_ms(step.get("delayMs"));
                match step.get("action").and_then(Value::as_str) {
                    Some("hang") => return,
                    Some("close") => {
                        self.later(delay, |_| std::process::exit(0));
                        return;
                    }
                    _ => {}
                }
                let fallback_items = self
                    .scenario
                    .get("diagnostics")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                self.later(delay, move |server| {
                    if let Some(error) = step.get("error") {
                        server.record(json!({
                            "type": "serverError", "method": "textDocument/diagnostic", "error": error,
                        }));
                        server.send(json!({
                            "jsonrpc": "2.0", "id": id,
                            "error": { "code": error["code"], "message": error["message"] },
                        }));
                        return;
                    }
                    let result = step
                        .get("report")
                        .cloned()
                        .unwrap_or_else(|| json!({ "items": fallback_items }));
                    server.record(json!({
                        "type": "serverResponse", "method": "textDocument/diagnostic", "result": result,
                    }));
                    server.send_response(&id, result);
                });
            }
            _ => self.send_response(&id, Value::Null),
        }
    }

    fn handle_notification(self: &Arc<Self>, message: &Value) {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        self.record(json!({
            "type": "clientNotification", "method": method,
            "params": message.get("params").cloned().unwrap_or(Value::Null),
        }));
        match method {
            "initialized" => {
                if let Some(edit) = self
                    .scenario
                    .get("unscopedApplyEdit")
                    .filter(|edit| !edit.is_null())
                {
                    self.send_apply_edit(edit, PendingRequest::Unscoped);
                }
                self.apply_publish_steps("initialized", None);
            }
            "textDocument/didOpen" => {
                self.apply_publish_steps(
                    "didOpen",
                    message.pointer("/params/textDocument/uri").cloned(),
                );
            }
            "textDocument/didChange" => {
                if let Some(diagnostics) = self
                    .scenario
                    .get("diagnostics")
                    .filter(|value| value.is_array())
                {
                    self.send(json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/publishDiagnostics",
                        "params": {
                            "uri": message.pointer("/params/textDocument/uri"),
                            "version": message.pointer("/params/textDocument/version"),
                            "diagnostics": diagnostics,
                        },
                    }));
                }
                self.apply_publish_steps(
                    "didChange",
                    message.pointer("/params/textDocument/uri").cloned(),
                );
            }
            "exit" => std::process::exit(0),
            _ => {}
        }
    }

    fn handle_message(self: &Arc<Self>, message: &Value) {
        let has_id = message.get("id").is_some();
        if has_id && (message.get("result").is_some() || message.get("error").is_some()) {
            self.handle_client_response(message);
        } else if has_id {
            self.handle_request(message);
        } else {
            self.handle_notification(message);
        }
    }
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = content_length else {
        std::process::exit(2);
    };
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, scenario_path, events_path] = args.as_slice() else {
        eprintln!("usage: lsp_core_workspace_edit_fixture <scenario.json> <events.jsonl>");
        std::process::exit(2);
    };
    let scenario = std::fs::read_to_string(scenario_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}));
    let server = Arc::new(Server {
        scenario,
        events_path: events_path.clone(),
        stdout: Mutex::new(std::io::stdout()),
        state: Mutex::new(State::default()),
    });
    let mut reader = BufReader::new(std::io::stdin());
    while let Some(message) = read_message(&mut reader) {
        server.handle_message(&message);
    }
}
