//! `rpc-host/task-parent-wire.ts`: adapt optional engine methods to the parent wire.
use crate::runners::task_parent_control::{TaskParentControlError, TaskParentWire};
use serde_json::Value;
use std::sync::Arc;

/// None means the pinned engine predates this operation. Engine refusals remain wire errors.
pub trait HostRunHoldClient: Send + Sync {
    fn set_client_info(&self, _command: &Value) -> Option<Result<Value, String>> {
        None
    }
    fn pause_for_run(&self, _command: &Value) -> Option<Result<Value, String>> {
        None
    }
    fn resume_for_run(&self, _command: &Value) -> Option<Result<Value, String>> {
        None
    }
    fn hold_input(&self, _command: &Value) -> Option<Result<Value, String>> {
        None
    }
    fn get_state(&self) -> Option<Result<Value, String>> {
        None
    }
}
pub fn create_host_task_parent_wire(client: Arc<dyn HostRunHoldClient>) -> Arc<TaskParentWire> {
    TaskParentWire::new(
        80,
        Arc::new(move |command| {
            let result = match command["type"].as_str() {
                Some("set_client_info") => client.set_client_info(&command),
                Some("pause_for_run") => client.pause_for_run(&command),
                Some("resume_for_run") => client.resume_for_run(&command),
                Some("hold_input") => client.hold_input(&command),
                Some("get_state") => client.get_state(),
                _ => None,
            }
            .ok_or_else(|| TaskParentControlError::new("unsupported"))?;
            Ok(match result {
                Ok(data) => serde_json::json!({"success":true,"data":data}),
                Err(error) => serde_json::json!({"success":false,"error":error}),
            })
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct OldEngine;
    impl HostRunHoldClient for OldEngine {}
    struct Engine;
    impl HostRunHoldClient for Engine {
        fn pause_for_run(&self, command: &Value) -> Option<Result<Value, String>> {
            assert_eq!(command["parentSessionId"], "parent");
            Some(Err("wrong_parent: another owner".into()))
        }
        fn get_state(&self) -> Option<Result<Value, String>> {
            Some(Ok(json!({"pause":{"pauseSeq":2,"records":[]}})))
        }
    }
    #[test]
    fn unavailable_methods_refuse_but_engine_errors_are_wire_answers() {
        let old = create_host_task_parent_wire(Arc::new(OldEngine));
        assert_eq!(
            (old.send)(json!({"type":"get_state"})).unwrap_err().code,
            "unsupported"
        );
        let current = create_host_task_parent_wire(Arc::new(Engine));
        assert_eq!(
            (current.send)(json!({"type":"pause_for_run","parentSessionId":"parent"}))
                .expect("answer"),
            json!({"success":false,"error":"wrong_parent: another owner"})
        );
        assert_eq!(
            (current.send)(json!({"type":"get_state"})).expect("state")["data"]["pause"]["pauseSeq"],
            2
        );
    }
}
