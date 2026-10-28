//! Durable synthetic human-attention occurrences (senpi lifecycle-attention.ts).
use indexmap::IndexMap;
use serde_json::{Value, json};

pub fn read_human_wait(value: &Value) -> Option<Value> {
    let kind = value.get("kind")?.as_str()?;
    if ![
        "question",
        "approval",
        "goal_blocked",
        "goal_paused",
        "stalled",
        "interrupted_run",
    ]
    .contains(&kind)
    {
        return None;
    }
    let mut result = json!({"kind":kind,"originSessionId":value.get("originSessionId")?.as_str()?,"requestId":value.get("requestId")?.as_str()?,"revision":value.get("revision")?.as_number()?,"since":value.get("since")?.as_str()?});
    for key in ["childSessionId", "childLabel"] {
        if let Some(text) = value[key].as_str() {
            result[key] = json!(text);
        }
    }
    Some(result)
}

pub struct LifecycleAttention {
    session_id: String,
    sequence: u64,
    open: IndexMap<String, Value>,
}
impl LifecycleAttention {
    pub fn new(session_id: &str, restored: Option<&Value>) -> Self {
        let mut result = Self {
            session_id: session_id.into(),
            sequence: restored
                .and_then(|r| r["attentionSeq"].as_u64())
                .unwrap_or(0),
            open: IndexMap::new(),
        };
        if let Some(items) = restored.and_then(|r| r["synthetic"].as_array()) {
            for item in items {
                if let (Some(key), Some(wait)) =
                    (item["key"].as_str(), read_human_wait(&item["wait"]))
                {
                    result.open.insert(key.into(), wait);
                }
            }
        }
        result
    }
    pub fn attention_seq(&self) -> u64 {
        self.sequence
    }
    pub fn synthetic(&self) -> Value {
        json!(
            self.open
                .iter()
                .map(|(key, wait)| json!({"key":key,"wait":wait}))
                .collect::<Vec<_>>()
        )
    }
    pub fn reconcile(&mut self, requests: &[(String, String)]) -> Vec<Value> {
        self.open
            .retain(|key, _| requests.iter().any(|(wanted, _)| wanted == key));
        requests.iter().map(|(key,kind)| {
            if let Some(wait) = self.open.get(key).filter(|w| w["kind"].as_str() == Some(kind.as_str())) { return wait.clone(); }
            self.sequence += 1;
            let wait = json!({"kind":kind,"originSessionId":self.session_id,"requestId":format!("{key}:{}", self.sequence),"revision":1,"since":chrono::Utc::now().to_rfc3339()});
            self.open.insert(key.clone(),wait.clone()); wait
        }).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_survives_restore_and_reopens() {
        let mut a = LifecycleAttention::new("s", None);
        let requests = vec![("stalled".into(), "stalled".into())];
        let first = a.reconcile(&requests);
        assert_eq!(first, a.reconcile(&requests));
        let saved = json!({"attentionSeq":a.attention_seq(),"synthetic":a.synthetic()});
        let mut b = LifecycleAttention::new("s", Some(&saved));
        assert_eq!(first, b.reconcile(&requests));
        b.reconcile(&[]);
        assert_ne!(
            first[0]["requestId"],
            b.reconcile(&requests)[0]["requestId"]
        );
    }
}
