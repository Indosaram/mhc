//! Hosted open validation and durable identity (`open-session.ts`).
use super::handle_port::{HostSessionIdentity, HostSessionPort};
use crate::{host::HostError, runners::pinned_model_equivalence::*};
use std::sync::Arc;
pub struct OpenedHostSession {
    pub session: HostSessionIdentity,
    pub attached: bool,
    pub durable_session_id: Option<String>,
    pub reported_model: Option<EffectiveModel>,
}
pub fn with_durable_id(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
pub fn open_task_host_session(
    client: Arc<dyn HostSessionPort>,
    mut opened: OpenedHostSession,
    pinned: Option<&CatalogModelIdentity>,
) -> Result<OpenedHostSession, HostError> {
    if opened.attached {
        return Ok(opened);
    }
    match client.get_state() {
        Ok(state) => {
            opened.durable_session_id = with_durable_id(&state["sessionId"]);
            let effective = state
                .get("model")
                .cloned()
                .and_then(|model| serde_json::from_value::<CatalogModelIdentity>(model).ok());
            if let Some(pin) = pinned {
                let catalog = if effective.as_ref().is_some_and(|model| {
                    differs_only_by_priority_alias(&model.identity(), &pin.identity())
                }) {
                    match client.get_available_models() {
                        Ok(catalog) => catalog,
                        Err(error) => {
                            if let Err(close) = client.close() {
                                eprintln!("failed pinned-session cleanup: {close}");
                            }
                            return Err(error);
                        }
                    }
                } else {
                    Vec::new()
                };
                let entry = catalog
                    .iter()
                    .find(|entry| entry.provider == pin.provider && entry.id == pin.id)
                    .unwrap_or(pin);
                if !effective.as_ref().is_some_and(|model| {
                    started_on_pinned_model(&model.identity(), &pin.identity(), Some(entry), None)
                        || catalog
                            .iter()
                            .find(|entry| entry.provider == model.provider && entry.id == model.id)
                            .is_some_and(|entry| {
                                is_priority_alias_of(Some(entry), &pin.identity(), None)
                            })
                }) {
                    if let Err(error) = client.close() {
                        eprintln!("failed pinned-session cleanup: {error}");
                    }
                    return Err(HostError {
                        message: "model_unavailable: refusing unverified or substituted host model"
                            .into(),
                    });
                }
            }
            opened.reported_model = effective.as_ref().and_then(|model| {
                reported_effective_model(Some(&model.identity()), state["serviceTier"].as_str())
            });
        }
        Err(error) => {
            if pinned.is_some() {
                if let Err(close) = client.close() {
                    eprintln!("failed pinned-session cleanup: {close}");
                }
                return Err(HostError {
                    message: format!("model_unavailable: effective model unreadable: {error}"),
                });
            }
            eprintln!("host session open state read failed: {error}");
        }
    }
    Ok(opened)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::child_handle::{ManagedChildListener, Unsubscribe};
    struct Port;
    impl HostSessionPort for Port {
        fn send(&self, _: serde_json::Value) -> Result<(), HostError> {
            Ok(())
        }
        fn get_state(&self) -> Result<serde_json::Value, HostError> {
            Ok(serde_json::json!({"sessionId":"durable"}))
        }
        fn on_event(&self, _: ManagedChildListener) -> Unsubscribe {
            Box::new(|| {})
        }
        fn close(&self) -> Result<(), HostError> {
            Ok(())
        }
        fn detach(&self) -> Result<(), HostError> {
            Ok(())
        }
    }
    #[test]
    fn fresh_unpinned_open_carries_nonempty_durable_id() {
        let opened = OpenedHostSession {
            session: HostSessionIdentity {
                routing_id: "route".into(),
                session_path: "path".into(),
                instance_id: "instance".into(),
            },
            attached: false,
            durable_session_id: None,
            reported_model: None,
        };
        let opened = open_task_host_session(Arc::new(Port), opened, None).unwrap();
        assert_eq!(opened.durable_session_id.as_deref(), Some("durable"));
        assert_eq!(with_durable_id(&serde_json::json!("")), None);
    }
}
