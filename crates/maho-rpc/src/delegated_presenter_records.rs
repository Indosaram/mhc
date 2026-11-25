//! Port of senpi `packages/coding-agent/src/modes/rpc/delegated-presenter-records.ts`.
//!
//! Private presenter records and presenter control discovery on the delegated-connection gate
//! (`senpi#4249` R-1b d1, d2). Outside `RPC_METHOD_CLASS`, so its digest is unchanged: a forwarder
//! that pins it keeps working.

use std::collections::HashSet;

use crate::delegation_wire::{DelegationVerdict, PrivateRefusal};
use crate::method_classes::RpcMethodClass;

pub const PRESENTER_CONTROL_INFO_COMMAND: &str = "mcp_presenter_control_info";

/// The presenter client records the private handler takes (`session-event-writer.ts`,
/// `mcp-presenter-forms.ts`).
const PRESENTER_CLIENT_RECORDS: [&str; 4] = [
    "mcp_presenter_context",
    "mcp_form_presented",
    "mcp_form_response",
    "mcp_form_sync",
];

/// A type a forwarder routes as private: its answer is always the private reply shape on a
/// delegated connection.
pub fn is_private_presenter_type(record_type: &str) -> bool {
    record_type.starts_with("mcp_form_") || record_type.starts_with("mcp_presenter_")
}

pub fn private_presenter_verdict(
    record_type: &str,
    id: Option<String>,
    granted: &HashSet<RpcMethodClass>,
) -> DelegationVerdict {
    if record_type == PRESENTER_CONTROL_INFO_COMMAND {
        return DelegationVerdict::Private {
            id,
            error: PrivateRefusal::PresenterNotAuthorized,
        };
    }
    if !PRESENTER_CLIENT_RECORDS.contains(&record_type) {
        return DelegationVerdict::Private {
            id,
            error: PrivateRefusal::DelegationUnclassified,
        };
    }
    if !granted.contains(&RpcMethodClass::Answer) {
        return DelegationVerdict::Private {
            id,
            error: PrivateRefusal::DelegationNotGranted,
        };
    }
    DelegationVerdict::Presenter { id }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_private_presenter_prefixes() {
        assert!(is_private_presenter_type("mcp_form_sync"));
        assert!(is_private_presenter_type("mcp_presenter_context"));
        assert!(is_private_presenter_type(PRESENTER_CONTROL_INFO_COMMAND));
        assert!(!is_private_presenter_type("prompt"));
    }

    #[test]
    fn decides_verdict_by_command_and_answer_grant() {
        let empty = HashSet::new();
        let with_answer = HashSet::from([RpcMethodClass::Answer]);

        assert_eq!(
            private_presenter_verdict(
                PRESENTER_CONTROL_INFO_COMMAND,
                Some("1".into()),
                &with_answer
            ),
            DelegationVerdict::Private {
                id: Some("1".into()),
                error: PrivateRefusal::PresenterNotAuthorized,
            }
        );
        assert_eq!(
            private_presenter_verdict("mcp_form_unknown", None, &with_answer),
            DelegationVerdict::Private {
                id: None,
                error: PrivateRefusal::DelegationUnclassified,
            }
        );
        assert_eq!(
            private_presenter_verdict("mcp_form_response", Some("2".into()), &empty),
            DelegationVerdict::Private {
                id: Some("2".into()),
                error: PrivateRefusal::DelegationNotGranted,
            }
        );
        assert_eq!(
            private_presenter_verdict("mcp_form_response", Some("2".into()), &with_answer),
            DelegationVerdict::Presenter {
                id: Some("2".into()),
            }
        );
    }
}
