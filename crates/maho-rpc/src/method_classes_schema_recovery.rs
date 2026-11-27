//! The schemas of the socket host's recovery and admission records (senpi#4272): pages under a checkpoint
//! token or an event lease, the lease receipt, and the attempt cancel and stream reset.

use crate::method_classes_schema_fields::{
    Schema, admission_session_schema, id_schema, literals_schema, object_schema, opt, req,
};

pub fn recovery_record_schemas() -> Vec<(&'static str, Schema)> {
    vec![
        (
            "get_recovery_page",
            Schema::Union(vec![
                admission_session_schema(
                    "get_recovery_page",
                    vec![
                        req("token", id_schema()),
                        req("admissionId", id_schema()),
                        req("componentRef", id_schema()),
                        req("requestId", id_schema()),
                        opt("cursor", id_schema()),
                    ],
                ),
                admission_session_schema(
                    "get_recovery_page",
                    vec![
                        req(
                            "lease",
                            object_schema(vec![
                                req("kind", literals_schema(&["event", "checkpoint"])),
                                req("id", id_schema()),
                            ]),
                        ),
                        req("admissionId", id_schema()),
                        req("componentRef", id_schema()),
                        req("requestId", id_schema()),
                        opt("cursor", id_schema()),
                    ],
                ),
            ]),
        ),
        (
            "release_recovery",
            admission_session_schema(
                "release_recovery",
                vec![
                    req(
                        "lease",
                        object_schema(vec![
                            req("kind", Schema::LiteralStr("event")),
                            req("id", id_schema()),
                        ]),
                    ),
                    req("admissionId", id_schema()),
                ],
            ),
        ),
        (
            "cancel_attempt",
            admission_session_schema("cancel_attempt", vec![req("admissionId", id_schema())]),
        ),
        (
            "reset_stream",
            admission_session_schema("reset_stream", vec![req("admissionId", id_schema())]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn recovery_records_require_admission_id() {
        let schemas = recovery_record_schemas();
        let get_page = &schemas
            .iter()
            .find(|(k, _)| *k == "get_recovery_page")
            .expect("get_recovery_page")
            .1;
        assert!(
            get_page
                .validate(
                    &json!({
                        "type": "get_recovery_page",
                        "lease": {"kind": "event", "id": "l1"},
                        "admissionId": "a1",
                        "componentRef": "c1",
                        "requestId": "r1"
                    }),
                    ""
                )
                .is_ok()
        );
        assert!(
            get_page
                .validate(
                    &json!({
                        "type": "get_recovery_page",
                        "token": "t1",
                        "componentRef": "c1",
                        "requestId": "r1"
                    }),
                    ""
                )
                .is_err()
        );
    }
}
