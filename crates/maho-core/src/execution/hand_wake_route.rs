//! Port of senpi packages/coding-agent/src/core/execution/hand-wake-route.ts.

use std::sync::Arc;

use maho_agent::StoreValue;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandWakeSource {
    Message,
    UserMessage,
    Entry,
    Notice,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HandWake {
    pub source: HandWakeSource,
    pub payload: StoreValue,
}

pub type WakeMessageFn = Arc<dyn Fn(Value, Option<Value>) + Send + Sync>;
pub type WakeEntryFn = Arc<dyn Fn(&str, Option<Value>) + Send + Sync>;
pub type WakeNotifyFn = Arc<dyn Fn(&str, Option<&str>) + Send + Sync>;

#[derive(Clone)]
pub struct LoopWakeRoute {
    pub send_message: WakeMessageFn,
    pub send_user_message: WakeMessageFn,
    pub append_entry: WakeEntryFn,
    pub notify: WakeNotifyFn,
}

pub fn loop_wake_route(
    offer: Arc<dyn Fn(HandWake) + Send + Sync>,
    session_id: Option<String>,
) -> LoopWakeRoute {
    let wake = move |source: HandWakeSource, mut obj: serde_json::Map<String, Value>| {
        if let Some(ref sid) = session_id {
            obj.insert("sessionId".to_owned(), json!(sid));
        }
        offer(HandWake {
            source,
            payload: Value::Object(obj),
        });
    };

    let w1 = wake.clone();
    let w2 = wake.clone();
    let w3 = wake.clone();
    let w4 = wake;

    LoopWakeRoute {
        send_message: Arc::new(move |message, options| {
            let mut obj = serde_json::Map::new();
            obj.insert("message".to_owned(), message);
            if let Some(opts) = options {
                obj.insert("options".to_owned(), opts);
            }
            w1(HandWakeSource::Message, obj);
        }),
        send_user_message: Arc::new(move |content, options| {
            let mut obj = serde_json::Map::new();
            obj.insert("content".to_owned(), content);
            if let Some(opts) = options {
                obj.insert("options".to_owned(), opts);
            }
            w2(HandWakeSource::UserMessage, obj);
        }),
        append_entry: Arc::new(move |custom_type, data| {
            let mut obj = serde_json::Map::new();
            obj.insert("customType".to_owned(), json!(custom_type));
            if let Some(d) = data {
                obj.insert("data".to_owned(), d);
            }
            w3(HandWakeSource::Entry, obj);
        }),
        notify: Arc::new(move |message, kind| {
            let mut obj = serde_json::Map::new();
            obj.insert("message".to_owned(), json!(message));
            if let Some(k) = kind {
                obj.insert("type".to_owned(), json!(k));
            }
            w4(HandWakeSource::Notice, obj);
        }),
    }
}

pub fn late_loop_route(
    wake: Option<Arc<dyn Fn(HandWake) + Send + Sync>>,
    dropped: Arc<dyn Fn(String) + Send + Sync>,
) -> Arc<dyn Fn(&str) -> LoopWakeRoute + Send + Sync> {
    if let Some(wake) = wake {
        return Arc::new(move |session_id| {
            loop_wake_route(Arc::clone(&wake), Some(session_id.to_owned()))
        });
    }
    Arc::new(move |session_id| {
        let drop_for = |what: &'static str,
                        sid: String,
                        dropped: Arc<dyn Fn(String) + Send + Sync>| {
            dropped(format!(
                "A {what} from loop session {sid}'s hook work arrived after its call ended and was dropped: this runtime has no wake route to the loop."
            ));
        };
        let s1 = session_id.to_owned();
        let d1 = Arc::clone(&dropped);
        let s2 = session_id.to_owned();
        let d2 = Arc::clone(&dropped);
        let s3 = session_id.to_owned();
        let d3 = Arc::clone(&dropped);
        let s4 = session_id.to_owned();
        let d4 = Arc::clone(&dropped);
        LoopWakeRoute {
            send_message: Arc::new(move |_, _| drop_for("message", s1.clone(), Arc::clone(&d1))),
            send_user_message: Arc::new(move |_, _| {
                drop_for("user message", s2.clone(), Arc::clone(&d2))
            }),
            append_entry: Arc::new(move |_, _| {
                drop_for("session entry", s3.clone(), Arc::clone(&d3))
            }),
            notify: Arc::new(move |_, _| drop_for("notice", s4.clone(), Arc::clone(&d4))),
        }
    })
}
