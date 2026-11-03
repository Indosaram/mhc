//! Port of senpi `terminal/rpc-monitor-render.ts`: how an RPC record reads as one monitor line.
use crate::rpc_monitor_coalesce::js_string;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

const UNKNOWN_JSON_LIMIT: usize = 160;

struct SessionView {
    label: String,
    status: String,
}

/// Upstream clears the per-batch "named once" set on the next microtask; the native port clears
/// it when the caller ends a delivery batch (`end_batch`), the same boundary.
#[derive(Default)]
pub struct RpcMonitorRenderer {
    sessions: HashMap<String, SessionView>,
    order: Vec<String>,
    named_this_batch: HashSet<String>,
}

pub fn session_key(socket: &str, record: &Value) -> String {
    format!("{socket}#{}", js_string(record.get("sessionId")))
}

fn context_task(record: &Value) -> Option<String> {
    record
        .get("context")
        .and_then(|context| context.get("task_id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn label_of(record: &Value) -> String {
    if let Some(task) = context_task(record) {
        return task;
    }
    if let Some(name) = record
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        return name.to_owned();
    }
    let durable = match record.get("durableSessionId").and_then(Value::as_str) {
        Some(durable) => durable.to_owned(),
        None => js_string(record.get("sessionId")),
    };
    durable.chars().take(8).collect()
}

fn model_id(model: Option<&Value>) -> Option<String> {
    model
        .and_then(Value::as_object)
        .and_then(|model| model.get("id"))
        .map(|id| js_string(Some(id)))
}

fn reason_of(outcome: Option<&Value>) -> Option<String> {
    outcome
        .and_then(Value::as_object)
        .and_then(|outcome| outcome.get("reason"))
        .map(|reason| js_string(Some(reason)))
}

fn count(value: Option<&Value>) -> String {
    match value {
        Some(Value::Array(items)) => items.len().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        _ => "0".to_owned(),
    }
}

fn tail(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    if chars.len() > 8 {
        chars[chars.len() - 8..].iter().collect()
    } else {
        id.to_owned()
    }
}

fn entry_type(record: &Value) -> String {
    let entry = record.get("entry").and_then(|entry| entry.get("type"));
    match entry.or_else(|| record.get("entryType")) {
        Some(value) if !value.is_null() => js_string(Some(value)),
        _ => "?".to_owned(),
    }
}

fn or_default(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null())
}

impl RpcMonitorRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remembers the sessions a subscription answer listed; returns the arm-time RESULT rows.
    pub fn seed(&mut self, socket: &str, snapshot: &[Value]) -> Vec<String> {
        snapshot
            .iter()
            .map(|row| {
                let status = js_string(row.get("status"));
                let view = self.view(socket, row);
                view.status = status;
                let model = model_id(row.get("model"));
                let attachments = row.get("attachments").and_then(Value::as_f64).map_or_else(
                    || "0".to_owned(),
                    |n| Value::from(n).to_string().trim_end_matches(".0").to_owned(),
                );
                format!(
                    "{} {} ({attachments} attached{})",
                    view.label,
                    view.status,
                    model.map_or_else(String::new, |model| format!(", model {model}"))
                )
            })
            .collect()
    }

    /// Drops every session an endpoint reported; returns how many.
    pub fn forget_socket(&mut self, socket: &str) -> usize {
        let prefix = format!("{socket}#");
        let before = self.sessions.len();
        self.sessions.retain(|key, _| !key.starts_with(&prefix));
        self.order.retain(|key| !key.starts_with(&prefix));
        before - self.sessions.len()
    }

    /// Every session this renderer knows, with its last status: what a checkpoint records.
    pub fn statuses(&self) -> BTreeMap<String, String> {
        self.order
            .iter()
            .filter_map(|key| {
                self.sessions
                    .get(key)
                    .map(|view| (key.clone(), view.status.clone()))
            })
            .collect()
    }

    /// Ends one delivery batch (upstream's microtask boundary).
    pub fn end_batch(&mut self) {
        self.named_this_batch.clear();
    }

    pub fn label(&mut self, socket: &str, record: &Value) -> String {
        let label = self.view(socket, record).label.clone();
        let key = session_key(socket, record);
        if self.named_this_batch.contains(&key) {
            return label;
        }
        self.named_this_batch.insert(key);
        format!("{label} ({})", js_string(record.get("sessionId")))
    }

    /// One line for a non-coalesced record, or `None` when it changes nothing worth a line.
    pub fn render(&mut self, socket: &str, record: &Value) -> Option<String> {
        let kind = js_string(record.get("type"));
        let line = match kind.as_str() {
            "session_status" => return self.status(socket, record),
            "effect_status" => {
                let detail = or_default(record.get("disposition"))
                    .map(|value| js_string(Some(value)))
                    .or_else(|| reason_of(record.get("outcome")));
                format!(
                    "rpc {} effect {} {} {}{}",
                    self.label(socket, record),
                    js_string(record.get("kind")),
                    tail(&js_string(record.get("effectId"))),
                    js_string(record.get("state")),
                    detail.map_or_else(String::new, |detail| format!(" ({detail})"))
                )
            }
            "agent_end" => format!(
                "rpc {} agent_end{}",
                self.label(socket, record),
                if record.get("aborted") == Some(&Value::Bool(true)) {
                    format!(
                        " aborted({})",
                        or_default(record.get("abortSource"))
                            .map_or_else(|| "user".to_owned(), |source| js_string(Some(source)))
                    )
                } else {
                    String::new()
                }
            ),
            "model_changed" => format!(
                "rpc {} model_changed -> {}{}",
                self.label(socket, record),
                model_id(record.get("model")).unwrap_or_else(|| "?".to_owned()),
                record
                    .get("source")
                    .map_or_else(String::new, |source| format!(
                        " ({})",
                        js_string(Some(source))
                    ))
            ),
            "queue_update" => format!(
                "rpc {} queue_update steering={} followUp={}",
                self.label(socket, record),
                count(record.get("steering")),
                count(record.get("followUp"))
            ),
            "session_replaced" => format!(
                "rpc {} session_replaced -> {}",
                self.label(socket, record),
                or_default(record.get("durableSessionId"))
                    .or_else(|| or_default(record.get("newSessionId")))
                    .map_or_else(|| "?".to_owned(), |id| js_string(Some(id)))
            ),
            "session_closed" => format!(
                "rpc {} session_closed {}",
                self.label(socket, record),
                or_default(record.get("reason")).map_or_else(String::new, |r| js_string(Some(r)))
            )
            .trim_end()
            .to_owned(),
            "extension_event" => format!(
                "rpc {} extension_event {}",
                self.label(socket, record),
                or_default(record.get("name"))
                    .or_else(|| or_default(record.get("event")))
                    .map_or_else(String::new, |r| js_string(Some(r)))
            )
            .trim_end()
            .to_owned(),
            "entry_appended" => format!(
                "rpc {} entry_appended {}",
                self.label(socket, record),
                entry_type(record)
            ),
            "tool_execution_start" | "tool_execution_end" => format!(
                "rpc {} tool {} {}",
                self.label(socket, record),
                or_default(record.get("toolName"))
                    .map_or_else(|| "?".to_owned(), |n| js_string(Some(n))),
                if kind == "tool_execution_start" {
                    "started"
                } else {
                    "ended"
                }
            ),
            "turn_end" | "turn_start" | "agent_start" | "agent_idle" | "agent_settled" => {
                format!("rpc {} {kind}", self.label(socket, record))
            }
            _ => {
                // Filter instead of `Map::remove`: under the workspace's serde_json `preserve_order`,
                // `remove` is `swap_remove` and would reorder the remaining keys.
                let rest: Map<String, Value> = record
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter(|(key, _)| key.as_str() != "type" && key.as_str() != "sessionId")
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                let json = Value::Object(rest).to_string();
                let chars: Vec<char> = json.chars().collect();
                let body = if chars.len() > UNKNOWN_JSON_LIMIT {
                    format!(
                        "{}...",
                        chars[..UNKNOWN_JSON_LIMIT].iter().collect::<String>()
                    )
                } else {
                    json
                };
                format!("rpc {} {kind} {body}", self.label(socket, record))
            }
        };
        Some(line)
    }

    fn status(&mut self, socket: &str, record: &Value) -> Option<String> {
        let to = js_string(record.get("status"));
        let view = self.view(socket, record);
        let from = std::mem::replace(&mut view.status, to.clone());
        if from == to {
            return None;
        }
        let why = record.get("lastChange").map_or_else(String::new, |change| {
            format!(
                " ({}, by {})",
                js_string(change.get("cause")),
                js_string(change.get("by"))
            )
        });
        Some(format!(
            "rpc {} status {from} -> {to}{why} seq={}",
            self.label(socket, record),
            js_string(record.get("seq"))
        ))
    }

    fn view(&mut self, socket: &str, record: &Value) -> &mut SessionView {
        let key = session_key(socket, record);
        let relabel =
            record.get("name").is_some_and(Value::is_string) || context_task(record).is_some();
        if !self.sessions.contains_key(&key) {
            self.order.push(key.clone());
            self.sessions.insert(
                key.clone(),
                SessionView {
                    label: label_of(record),
                    status: "unknown".to_owned(),
                },
            );
        } else if relabel {
            self.sessions.get_mut(&key).expect("present").label = label_of(record);
        }
        self.sessions.get_mut(&key).expect("present")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn seeds_statuses_and_names_a_session_once_per_batch() {
        let mut renderer = RpcMonitorRenderer::new();
        let rows = renderer.seed(
            "h",
            &[json!({"sessionId":"r1","durableSessionId":"0123456789","status":"idle","attachments":2,"model":{"id":"m"}})],
        );
        assert_eq!(rows, vec!["01234567 idle (2 attached, model m)".to_owned()]);
        let status = json!({"type":"session_status","sessionId":"r1","status":"busy","seq":3,"lastChange":{"cause":"prompt","by":"user"}});
        assert_eq!(
            renderer.render("h", &status).as_deref(),
            Some("rpc 01234567 (r1) status idle -> busy (prompt, by user) seq=3")
        );
        assert_eq!(renderer.render("h", &status), None);
        assert_eq!(
            renderer
                .render(
                    "h",
                    &json!({"type":"agent_end","sessionId":"r1","aborted":true})
                )
                .as_deref(),
            Some("rpc 01234567 agent_end aborted(user)")
        );
        renderer.end_batch();
        assert_eq!(
            renderer
                .render("h", &json!({"type":"turn_end","sessionId":"r1"}))
                .as_deref(),
            Some("rpc 01234567 (r1) turn_end")
        );
        assert_eq!(
            renderer.statuses().get("h#r1").map(String::as_str),
            Some("busy")
        );
        assert_eq!(renderer.forget_socket("h"), 1);
    }

    #[test]
    fn task_label_and_unknown_kind_json() {
        let mut renderer = RpcMonitorRenderer::new();
        let record =
            json!({"type":"custom_kind","sessionId":"r2","context":{"task_id":"st_9"},"x":1});
        assert_eq!(
            renderer.render("h", &record).as_deref(),
            Some(r#"rpc st_9 (r2) custom_kind {"context":{"task_id":"st_9"},"x":1}"#)
        );
        assert_eq!(session_key("h", &record), "h#r2");
    }
}
