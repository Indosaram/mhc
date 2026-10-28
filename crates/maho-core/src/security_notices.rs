//! Generation-stamped metadata-only security notice snapshots.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecurityNoticeCategory {
    #[serde(rename = "skill-link-blocked")]
    SkillLinkBlocked,
    #[serde(rename = "mcp-env-withheld")]
    McpEnvWithheld,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityNotice {
    pub category: SecurityNoticeCategory,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecurityNoticeState {
    pub generation: u64,
    pub notices: Vec<SecurityNotice>,
}
pub fn is_security_notice_state(value: &Value) -> bool {
    value
        .get("generation")
        .and_then(Value::as_u64)
        .is_some_and(|n| n <= 9_007_199_254_740_991)
        && serde_json::from_value::<SecurityNoticeState>(value.clone()).is_ok()
}
type Listener = Arc<dyn Fn(SecurityNoticeState) + Send + Sync>;
#[derive(Default)]
pub struct SecurityNoticeSnapshot {
    notices: Vec<(String, SecurityNotice)>,
    generation: u64,
    listeners: Arc<Mutex<Vec<(u64, Listener)>>>,
    next_id: u64,
}
pub struct SecurityNoticeSubscription {
    id: u64,
    listeners: Arc<Mutex<Vec<(u64, Listener)>>>,
}
impl Drop for SecurityNoticeSubscription {
    fn drop(&mut self) {
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(id, _)| *id != self.id);
    }
}
impl SecurityNoticeSnapshot {
    pub fn snapshot(&self) -> SecurityNoticeState {
        SecurityNoticeState {
            generation: self.generation,
            notices: self.notices.iter().map(|(_, n)| n.clone()).collect(),
        }
    }
    pub fn set_resources(&mut self, diagnostics: &[Value]) {
        let before = self.notices.clone();
        self.notices
            .retain(|(key, _)| !key.starts_with("resource:"));
        for diagnostic in diagnostics {
            if diagnostic.get("category").and_then(Value::as_str) != Some("skill-link-blocked") {
                continue;
            }
            if let Ok(notice) = serde_json::from_value::<SecurityNotice>(diagnostic.clone()) {
                let key = format!(
                    "resource:{}",
                    notice.path.as_ref().unwrap_or(&notice.message)
                );
                self.put(key, notice);
            }
        }
        if before != self.notices {
            self.publish();
        }
    }
    fn put(&mut self, key: String, notice: SecurityNotice) {
        if let Some((_, n)) = self.notices.iter_mut().find(|(k, _)| k == &key) {
            *n = notice;
        } else {
            self.notices.push((key, notice));
        }
    }
    pub fn add(&mut self, notice: SecurityNotice) {
        let category = match notice.category {
            SecurityNoticeCategory::SkillLinkBlocked => "skill-link-blocked",
            SecurityNoticeCategory::McpEnvWithheld => "mcp-env-withheld",
        };
        let key = format!(
            "runtime:{category}:{}",
            notice.path.as_ref().unwrap_or(&notice.message)
        );
        if self.notices.iter().any(|(k, n)| k == &key && n == &notice) {
            return;
        }
        self.put(key, notice);
        self.publish();
    }
    pub fn accept(&mut self, value: &Value) {
        let Ok(notice) = serde_json::from_value::<SecurityNotice>(value.clone()) else {
            return;
        };
        if value.get("remove") == Some(&Value::Bool(true)) && notice.path.is_some() {
            let category = value["category"].as_str().unwrap_or_default();
            let key = format!(
                "runtime:{category}:{}",
                notice.path.as_deref().unwrap_or_default()
            );
            let before = self.notices.len();
            self.notices.retain(|(k, _)| k != &key);
            if before != self.notices.len() {
                self.publish();
            }
        } else {
            self.add(notice);
        }
    }
    pub fn subscribe(&mut self, listener: Listener) -> SecurityNoticeSubscription {
        let id = self.next_id;
        self.next_id += 1;
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, listener.clone()));
        listener(self.snapshot());
        SecurityNoticeSubscription {
            id,
            listeners: self.listeners.clone(),
        }
    }
    pub fn dispose(&mut self) {
        self.notices.clear();
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
    fn publish(&mut self) {
        self.generation += 1;
        let listeners = self
            .listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for (_, listener) in listeners {
            listener(self.snapshot());
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn deduplication_removal_and_validation() {
        let mut s = SecurityNoticeSnapshot::default();
        let n = json!({"category":"mcp-env-withheld","message":"withheld","path":"server"});
        s.accept(&n);
        s.accept(&n);
        assert_eq!(s.snapshot().generation, 1);
        s.accept(&json!({"category":"mcp-env-withheld","message":"withheld","path":"server","remove":true}));
        assert!(s.snapshot().notices.is_empty());
        assert_eq!(s.snapshot().generation, 2);
        assert!(!is_security_notice_state(
            &json!({"generation":-1,"notices":[]})
        ));
    }
}
