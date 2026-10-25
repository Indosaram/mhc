//! The mutable durable identity (`rpc-host/handle-session-identity.ts`).
use super::handle_port::{HostSessionIdentity, HostSessionReplaced};
use std::sync::{Arc, Mutex};
pub struct HandleSessionIdentity {
    current: Mutex<(HostSessionIdentity, Option<String>)>,
    learned: Arc<dyn Fn(&str) + Send + Sync>,
}
pub fn create_handle_session_identity(
    initial: HostSessionIdentity,
    initial_durable_id: Option<String>,
    learned: Arc<dyn Fn(&str) + Send + Sync>,
) -> HandleSessionIdentity {
    HandleSessionIdentity {
        current: Mutex::new((initial, initial_durable_id)),
        learned,
    }
}
impl HandleSessionIdentity {
    pub fn current(&self) -> HostSessionIdentity {
        self.current.lock().expect("hosted child state lock poisoned").0.clone()
    }
    pub fn durable_id(&self) -> Option<String> {
        self.current.lock().expect("hosted child state lock poisoned").1.clone()
    }
    pub fn adopt(&self, next: HostSessionIdentity) {
        self.current.lock().expect("hosted child state lock poisoned").0 = next;
    }
    pub fn observe_durable_id(&self, id: &serde_json::Value) {
        let Some(id) = id.as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let mut current = self.current.lock().expect("hosted child state lock poisoned");
        if current.1.is_some() {
            return;
        }
        current.1 = Some(id.into());
        drop(current);
        (self.learned)(id);
    }
    pub fn follow_replace(&self, event: HostSessionReplaced) -> HostSessionReplaced {
        let mut current = self.current.lock().expect("hosted child state lock poisoned");
        current.0.session_path = event.session_file.clone();
        current.1 = Some(event.durable_session_id.clone());
        event
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delayed_heartbeat_never_replaces_known_or_replaced_identity() {
        let (tx, rx) = std::sync::mpsc::channel();
        let identity = create_handle_session_identity(
            HostSessionIdentity {
                routing_id: "route".into(),
                session_path: "old".into(),
                instance_id: "instance".into(),
            },
            None,
            Arc::new(move |id| tx.send(id.to_string()).unwrap()),
        );
        identity.observe_durable_id(&serde_json::json!("first"));
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap(),
            "first"
        );
        identity.follow_replace(HostSessionReplaced {
            session_file: "new".into(),
            durable_session_id: "next".into(),
        });
        identity.observe_durable_id(&serde_json::json!("late"));
        assert_eq!(identity.durable_id().as_deref(), Some("next"));
        assert!(rx.try_recv().is_err());
    }
}
