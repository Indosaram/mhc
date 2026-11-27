//! Port of senpi packages/coding-agent/src/core/session-file-call-keys.ts.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::session_call_keys::block_call_key;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ResultCallMark {
    RuntimeItemKey(String),
    CallUnknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct EntryCallKeys {
    parts: Option<Vec<Option<String>>>,
    result: Option<ResultCallMark>,
}

fn role_of(entry: &Value) -> Option<&str> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    entry.get("message")?.get("role")?.as_str()
}

fn take_keys(entry: &mut Value) -> Option<EntryCallKeys> {
    let role = role_of(entry)?.to_owned();
    if role == "assistant" {
        let message = entry.get_mut("message")?.as_object_mut()?;
        let parts = message.get_mut("content")?.as_array_mut()?;
        let mut collected = Vec::with_capacity(parts.len());
        let mut any = false;
        for part in parts.iter_mut() {
            let key = part
                .as_object_mut()
                .and_then(|obj| obj.remove("runtimeItemKey"))
                .and_then(|v| match v {
                    Value::String(s) => Some(s),
                    _ => None,
                });
            if key.is_some() {
                any = true;
            }
            collected.push(key);
        }
        return any.then_some(EntryCallKeys {
            parts: Some(collected),
            result: None,
        });
    }
    if role != "toolResult" {
        return None;
    }
    let obj = entry.as_object_mut()?;
    let key = obj.remove("runtimeItemKey");
    let unknown = obj.remove("callUnknown");
    if unknown == Some(Value::Bool(true)) {
        return Some(EntryCallKeys {
            parts: None,
            result: Some(ResultCallMark::CallUnknown),
        });
    }
    if let Some(Value::String(s)) = key {
        return Some(EntryCallKeys {
            parts: None,
            result: Some(ResultCallMark::RuntimeItemKey(s)),
        });
    }
    None
}

/// The out-of-band call keys of one session file (RULING-126/127, senpi#5374).
///
/// The file holds `runtimeItemKey` on each `toolCall` part and `runtimeItemKey` or
/// `callUnknown: true` on a `toolResult` entry; memory never does, so legacy readers
/// keep their bytes.
#[derive(Clone, Debug, Default)]
pub struct SessionFileCallKeys {
    by_entry: HashMap<String, EntryCallKeys>,
}

impl SessionFileCallKeys {
    pub fn clear(&mut self) {
        self.by_entry.clear();
    }

    pub fn adopt(&mut self, entries: &mut [Value]) {
        for entry in entries.iter_mut() {
            if let Some(keys) = take_keys(entry)
                && let Some(id) = entry.get("id").and_then(Value::as_str)
            {
                self.by_entry.insert(id.to_owned(), keys);
            }
        }
    }

    /// An entry read again from the file (evicted string recovery): the table already holds its keys.
    pub fn strip(&self, mut entry: Value) -> Value {
        let _ = take_keys(&mut entry);
        entry
    }

    /// A new assistant entry: each toolCall part keyed by its block or bridged start key.
    pub fn record_assistant(
        &mut self,
        entry: &Value,
        owner: Option<&str>,
        bridged: Option<&HashMap<usize, String>>,
    ) {
        let Some(entry_id) = entry.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(parts) = entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
        else {
            return;
        };
        let fallback_prefix;
        let prefix = match owner {
            Some(o) => o,
            None => {
                fallback_prefix = uuid::Uuid::new_v4().to_string();
                &fallback_prefix
            }
        };
        let keys: Vec<Option<String>> = parts
            .iter()
            .enumerate()
            .map(|(index, part)| {
                if part.get("type").and_then(Value::as_str) == Some("toolCall") {
                    Some(
                        bridged
                            .and_then(|b| b.get(&index).cloned())
                            .unwrap_or_else(|| block_call_key(prefix, index)),
                    )
                } else {
                    None
                }
            })
            .collect();
        if keys.iter().any(Option::is_some) {
            self.by_entry.insert(
                entry_id.to_owned(),
                EntryCallKeys {
                    parts: Some(keys),
                    result: None,
                },
            );
        }
    }

    /// A new toolResult entry appended under `leaf_id`.
    pub fn record_result<'a>(
        &mut self,
        entry: &Value,
        leaf_id: Option<&str>,
        entry_of: &dyn Fn(&str) -> Option<&'a Value>,
    ) {
        let Some(entry_id) = entry.get("id").and_then(Value::as_str) else {
            return;
        };
        let id = entry
            .get("message")
            .filter(|m| m.get("role").and_then(Value::as_str) == Some("toolResult"))
            .and_then(|m| m.get("toolCallId"))
            .and_then(Value::as_str);

        let mut closed = HashSet::new();
        let mut assistant: Option<&'a Value> = None;
        let mut cursor = leaf_id.and_then(entry_of);
        while let Some(at) = cursor {
            if role_of(at) == Some("assistant") {
                assistant = Some(at);
                break;
            }
            if let Some(msg) = at.get("message")
                && at.get("type").and_then(Value::as_str) == Some("message")
                && msg.get("role").and_then(Value::as_str) == Some("toolResult")
                && let Some(tc_id) = msg.get("toolCallId").and_then(Value::as_str)
            {
                closed.insert(tc_id.to_owned());
            }
            cursor = at
                .get("parentId")
                .and_then(Value::as_str)
                .and_then(entry_of);
        }

        let matches: Vec<usize> = assistant
            .and_then(|a| a.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .enumerate()
                    .filter_map(|(index, part)| {
                        if part.get("type").and_then(Value::as_str) == Some("toolCall")
                            && part.get("id").and_then(Value::as_str) == id
                        {
                            Some(index)
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let open = id.is_some_and(|tc_id| matches.len() == 1 && !closed.contains(tc_id));
        let key = if open
            && let Some(assistant_entry) = assistant
            && let Some(assistant_id) = assistant_entry.get("id").and_then(Value::as_str)
            && let Some(idx) = matches.first().copied()
        {
            self.by_entry
                .get(assistant_id)
                .and_then(|ek| ek.parts.as_ref())
                .and_then(|parts| parts.get(idx))
                .and_then(Clone::clone)
        } else {
            None
        };

        let result = match key {
            Some(k) => ResultCallMark::RuntimeItemKey(k),
            None => ResultCallMark::CallUnknown,
        };
        self.by_entry.insert(
            entry_id.to_owned(),
            EntryCallKeys {
                parts: None,
                result: Some(result),
            },
        );
    }

    /// The entry as the file holds it: its persisted keys back in place.
    pub fn decorate(&self, entry: Value) -> Value {
        self.decorate_cow(Cow::Owned(entry)).into_owned()
    }

    pub(crate) fn decorate_cow<'a>(&self, mut entry: Cow<'a, Value>) -> Cow<'a, Value> {
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            return entry;
        }
        let Some(keys) = entry
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| self.by_entry.get(id))
        else {
            return entry;
        };
        let value = entry.to_mut();
        if let Some(result) = &keys.result {
            if let Some(obj) = value.as_object_mut() {
                match result {
                    ResultCallMark::RuntimeItemKey(k) => {
                        obj.insert("runtimeItemKey".into(), Value::String(k.clone()));
                    }
                    ResultCallMark::CallUnknown => {
                        obj.insert("callUnknown".into(), Value::Bool(true));
                    }
                }
            }
            return entry;
        }
        if let Some(part_keys) = &keys.parts
            && let Some(parts) = value
                .get_mut("message")
                .and_then(Value::as_object_mut)
                .and_then(|m| m.get_mut("content"))
                .and_then(Value::as_array_mut)
        {
            for (index, part) in parts.iter_mut().enumerate() {
                if let Some(Some(key)) = part_keys.get(index)
                    && let Some(obj) = part.as_object_mut()
                {
                    // Re-insert so persisted key ordering puts runtimeItemKey after other fields
                    let mut rebuilt = Map::with_capacity(obj.len() + 1);
                    for (k, v) in obj.iter() {
                        rebuilt.insert(k.clone(), v.clone());
                    }
                    rebuilt.insert("runtimeItemKey".into(), Value::String(key.clone()));
                    *obj = rebuilt;
                }
            }
        }
        entry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strips_on_adopt_and_decorates_on_write_and_tracks_open_results() {
        let mut keys = SessionFileCallKeys::default();
        let assistant = json!({
            "type": "message",
            "id": "e1",
            "parentId": null,
            "message": {
                "role": "assistant",
                "content": [{ "type": "toolCall", "id": "call1", "name": "bash", "arguments": {} }]
            }
        });
        keys.record_assistant(&assistant, Some("msg-1"), None);
        let decorated_assistant = keys.decorate(assistant.clone());
        assert_eq!(
            decorated_assistant["message"]["content"][0]["runtimeItemKey"],
            "blk:msg-1:0"
        );

        let result_entry = json!({
            "type": "message",
            "id": "e2",
            "parentId": "e1",
            "message": { "role": "toolResult", "toolCallId": "call1", "content": [] }
        });
        let map = HashMap::from([("e1".to_owned(), assistant.clone())]);
        keys.record_result(&result_entry, Some("e1"), &|id| map.get(id));
        let decorated_res = keys.decorate(result_entry.clone());
        assert_eq!(decorated_res["runtimeItemKey"], "blk:msg-1:0");

        // Second result for already closed call gets callUnknown
        let dup_res = json!({
            "type": "message",
            "id": "e3",
            "parentId": "e2",
            "message": { "role": "toolResult", "toolCallId": "call1", "content": [] }
        });
        let map2 = HashMap::from([
            ("e1".to_owned(), assistant),
            ("e2".to_owned(), result_entry),
        ]);
        keys.record_result(&dup_res, Some("e2"), &|id| map2.get(id));
        let decorated_dup = keys.decorate(dup_res);
        assert_eq!(decorated_dup["callUnknown"], true);
    }
}
