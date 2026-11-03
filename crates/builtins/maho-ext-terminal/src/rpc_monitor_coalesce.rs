//! Port of senpi `terminal/rpc-monitor-coalesce.ts`: streaming records collapse per session into
//! one line per 2 s window, surface churn into one line per session per 5 s.
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub const DELTA_WINDOW_MS: u64 = 2_000;
pub const SURFACE_WINDOW_MS: u64 = 5_000;
const DELTA_KINDS: [&str; 3] = [
    "message_update",
    "tool_execution_update",
    "bash_execution_update",
];
const SURFACE_KINDS: [&str; 2] = ["loaded_surfaces_changed", "commands_changed"];
const LAST_TEXT_LIMIT: usize = 60;

struct Window {
    count: u64,
    last: String,
    timer: tokio::task::JoinHandle<()>,
}

type Flush = Arc<dyn Fn(String) + Send + Sync>;

pub struct RpcMonitorCoalescer {
    windows: Arc<Mutex<HashMap<String, Window>>>,
    flush: Flush,
}

/// JavaScript `String(value)` for the record fields this module and the renderer print.
pub(crate) fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) => "null".to_owned(),
        Some(Value::Object(_)) => "[object Object]".to_owned(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(other) => other.to_string(),
    }
}

impl RpcMonitorCoalescer {
    pub fn new(flush: impl Fn(String) + Send + Sync + 'static) -> Self {
        Self {
            windows: Arc::new(Mutex::new(HashMap::new())),
            flush: Arc::new(flush),
        }
    }

    /// Takes a coalesced kind into its window; false for every other kind (the caller renders it).
    /// Must run inside a tokio runtime: the window close is a spawned timer.
    pub fn accept(
        &self,
        session_key: &str,
        label: impl Fn() -> String + Send + 'static,
        record: &Value,
    ) -> bool {
        let kind = js_string(record.get("type"));
        let delta = DELTA_KINDS.contains(&kind.as_str());
        if !delta && !SURFACE_KINDS.contains(&kind.as_str()) {
            return false;
        }
        let key = if delta {
            format!("{session_key}#deltas")
        } else {
            format!("{session_key}#{kind}")
        };
        let last = if delta {
            last_text(record)
        } else {
            String::new()
        };
        let mut windows = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(open) = windows.get_mut(&key) {
            open.count += 1;
            if !last.is_empty() {
                open.last = last;
            }
            return true;
        }
        let shared = self.windows.clone();
        let flush = self.flush.clone();
        let timer_key = key.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(if delta {
                DELTA_WINDOW_MS
            } else {
                SURFACE_WINDOW_MS
            }))
            .await;
            let Some(window) = shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&timer_key)
            else {
                return;
            };
            flush(if delta {
                format!(
                    "rpc {} streaming +{} deltas{}",
                    label(),
                    window.count,
                    if window.last.is_empty() {
                        String::new()
                    } else {
                        format!(" (last: \"{}\")", window.last)
                    }
                )
            } else {
                format!("rpc {} {kind} x{}", label(), window.count)
            });
        });
        windows.insert(
            key,
            Window {
                count: 1,
                last,
                timer,
            },
        );
        true
    }

    pub fn dispose(&self) {
        for (_, window) in self
            .windows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            window.timer.abort();
        }
    }
}

fn last_text(record: &Value) -> String {
    let event = record.get("assistantMessageEvent");
    // Only answer text: a thinking delta is not what the session is saying.
    let text = match (
        event.and_then(|e| e.get("type")).and_then(Value::as_str),
        event.and_then(|e| e.get("delta")).and_then(Value::as_str),
    ) {
        (Some("text_delta"), Some(delta)) => delta,
        _ => record.get("output").and_then(Value::as_str).unwrap_or(""),
    };
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // JavaScript slices UTF-16 units; char-based slicing keeps the result valid UTF-8.
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() > LAST_TEXT_LIMIT {
        chars[chars.len() - LAST_TEXT_LIMIT..].iter().collect()
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn delta(text: &str) -> Value {
        json!({"type":"message_update","sessionId":"s1","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":text}})
    }

    fn collector() -> (Arc<Mutex<Vec<String>>>, RpcMonitorCoalescer) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let coalescer = RpcMonitorCoalescer::new(move |line| sink.lock().unwrap().push(line));
        (lines, coalescer)
    }

    async fn advance(ms: u64) {
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(std::time::Duration::from_millis(ms)).await;
        for _ in 0..8 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn folds_n_deltas_in_one_window_into_one_line_counting_n_with_the_last_text() {
        let (lines, coalescer) = collector();
        for text in ["Hel", "lo ", "wor", "ld"] {
            assert!(coalescer.accept("h#s1", || "label".into(), &delta(text)));
        }
        advance(DELTA_WINDOW_MS - 1).await;
        assert!(lines.lock().unwrap().is_empty());
        advance(1).await;
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["rpc label streaming +4 deltas (last: \"ld\")".to_owned()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn opens_a_new_window_after_one_closes_and_keeps_sessions_apart() {
        let (lines, coalescer) = collector();
        coalescer.accept("h#s1", || "one".into(), &delta("a"));
        let mut second = delta("b");
        second["sessionId"] = json!("s2");
        coalescer.accept("h#s2", || "two".into(), &second);
        advance(DELTA_WINDOW_MS).await;
        coalescer.accept("h#s1", || "one".into(), &delta("c"));
        advance(DELTA_WINDOW_MS).await;
        let mut got = lines.lock().unwrap().clone();
        got[..2].sort();
        assert_eq!(
            got,
            vec![
                "rpc one streaming +1 deltas (last: \"a\")".to_owned(),
                "rpc two streaming +1 deltas (last: \"b\")".to_owned(),
                "rpc one streaming +1 deltas (last: \"c\")".to_owned(),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn collapses_surface_churn_per_5_s_and_leaves_every_other_kind_to_the_renderer() {
        let (lines, coalescer) = collector();
        let churn = json!({"type":"commands_changed","sessionId":"s1"});
        coalescer.accept("h#s1", || "one".into(), &churn);
        coalescer.accept("h#s1", || "one".into(), &churn);
        let passed = coalescer.accept(
            "h#s1",
            || "one".into(),
            &json!({"type":"session_status","sessionId":"s1"}),
        );
        advance(SURFACE_WINDOW_MS).await;
        assert!(!passed);
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["rpc one commands_changed x2".to_owned()]
        );
    }
}
