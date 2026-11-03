//! senpi `modes/rpc/session-structured-lanes.ts`: per-session receivers of the internal structured
//! lane (senpi#4272). The host-side owner of seeded records and block controls (S03/S04) registers here.
//! Legacy sockets, stdout and the replay snapshot never read it.
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

pub type StructuredReceiver = Arc<dyn Fn(&Value) + Send + Sync>;
type SessionReceivers = HashMap<String, Vec<(u64, StructuredReceiver)>>;

#[derive(Clone, Default)]
pub struct SessionStructuredLanes {
    receivers: Arc<Mutex<SessionReceivers>>,
    next: Arc<Mutex<u64>>,
}

impl SessionStructuredLanes {
    /// Registers `receiver` for `session_id`; the returned closure unregisters it.
    pub fn observe(
        &self,
        session_id: &str,
        receiver: StructuredReceiver,
    ) -> impl FnOnce() + Send + 'static {
        let id = {
            let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
            *next += 1;
            *next
        };
        self.receivers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(session_id.to_owned())
            .or_default()
            .push((id, receiver));
        let receivers = self.receivers.clone();
        let session_id = session_id.to_owned();
        move || {
            let mut receivers = receivers.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(list) = receivers.get_mut(&session_id) {
                list.retain(|(entry, _)| *entry != id);
                if list.is_empty() {
                    receivers.remove(&session_id);
                }
            }
        }
    }

    pub fn accept(&self, session_id: &str, record: &Value) {
        let receivers: Vec<StructuredReceiver> = self
            .receivers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(session_id)
            .map(|list| list.iter().map(|(_, receiver)| receiver.clone()).collect())
            .unwrap_or_default();
        for receiver in receivers {
            receiver(record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delivers_per_session_until_unobserved() {
        let lanes = SessionStructuredLanes::default();
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log = seen.clone();
        let stop = lanes.observe(
            "s",
            Arc::new(move |record| log.lock().expect("log").push(record.clone())),
        );
        lanes.accept("s", &json!({"n":1}));
        lanes.accept("other", &json!({"n":2}));
        stop();
        lanes.accept("s", &json!({"n":3}));
        assert_eq!(*seen.lock().expect("log"), [json!({"n":1})]);
        assert!(lanes.receivers.lock().expect("map").is_empty());
    }
}
