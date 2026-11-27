//! Port of senpi `packages/coding-agent/src/modes/rpc/delegated-drive-origin.ts`.

use maho_core::turn_origin::ConnectionOrigin;
use serde_json::Value;

use crate::method_classes::{RpcMethodClass, classify_rpc_method};

fn is_acting_class(class: RpcMethodClass) -> bool {
    matches!(class, RpcMethodClass::Drive | RpcMethodClass::DeviceAdmin)
}

/// The origin a record's own handling runs under (`senpi#6342`): delegated for a cross-account
/// record that can act on the session's work, decided by the engine's own method-class table
/// (`drive`, and `device_admin`, which grants more), never by a hand-kept list of commands, so a
/// new run starter (`continue_from_leaf`, `wake`, `resume_for_run`, ...) cannot slip past the rule.
/// Any other record runs as local.
pub fn record_turn_origin(command: &Value, origin: ConnectionOrigin) -> ConnectionOrigin {
    if origin != ConnectionOrigin::Delegated {
        return ConnectionOrigin::Local;
    }
    match classify_rpc_method(command) {
        Some(method_class) if is_acting_class(method_class) => ConnectionOrigin::Delegated,
        _ => ConnectionOrigin::Local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn acting_classes_stay_delegated_while_observe_and_local_run_local() {
        assert_eq!(
            record_turn_origin(
                &json!({"type": "prompt", "message": "hi"}),
                ConnectionOrigin::Delegated
            ),
            ConnectionOrigin::Delegated
        );
        assert_eq!(
            record_turn_origin(
                &json!({"type": "bash", "command": "ls"}),
                ConnectionOrigin::Delegated
            ),
            ConnectionOrigin::Delegated
        );
        assert_eq!(
            record_turn_origin(&json!({"type": "get_state"}), ConnectionOrigin::Delegated),
            ConnectionOrigin::Local
        );
        assert_eq!(
            record_turn_origin(
                &json!({"type": "prompt", "message": "hi"}),
                ConnectionOrigin::Local
            ),
            ConnectionOrigin::Local
        );
        assert_eq!(
            record_turn_origin(&json!({"type": "unknown_cmd"}), ConnectionOrigin::Delegated),
            ConnectionOrigin::Local
        );
    }
}
