//! The schema of `admit_external_message`, a relayed peer-agent message admitted as a delivery (senpi#6233).

use crate::method_classes_schema_fields::{
    Schema, id_schema, literals_schema, opt, pos_int_schema, req, session_schema, str_schema,
    strict_object_schema,
};

fn delivery_sender_schema() -> Schema {
    Schema::Union(vec![
        strict_object_schema(vec![
            req("kind", Schema::LiteralStr("agent")),
            req("session_id", id_schema()),
            opt("name", id_schema()),
        ]),
        strict_object_schema(vec![
            req("kind", Schema::LiteralStr("command_line")),
            opt("user", id_schema()),
        ]),
        strict_object_schema(vec![
            req("kind", Schema::LiteralStr("external")),
            req("platform", id_schema()),
            opt("author", id_schema()),
        ]),
    ])
}

pub fn delivery_record_schemas() -> Vec<(&'static str, Schema)> {
    vec![(
        "admit_external_message",
        session_schema(
            "admit_external_message",
            vec![
                req("deliveryId", id_schema()),
                req("text", str_schema()),
                opt("sender", delivery_sender_schema()),
                opt("displayText", str_schema()),
                req("deliverAs", literals_schema(&["steer", "followUp"])),
                opt("expectedTurnId", id_schema()),
                opt("whenIdle", literals_schema(&["start", "queue"])),
                opt("chainCount", pos_int_schema()),
            ],
        ),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn admit_external_message_accepts_valid_senders_and_rejects_extra_sender_fields() {
        let (_, schema) = &delivery_record_schemas()[0];
        assert!(
            schema
                .validate(
                    &json!({
                        "type": "admit_external_message",
                        "deliveryId": "d1",
                        "text": "hello",
                        "deliverAs": "steer",
                        "sender": {"kind": "agent", "session_id": "s1", "name": "worker"}
                    }),
                    ""
                )
                .is_ok()
        );
        assert!(
            schema
                .validate(
                    &json!({
                        "type": "admit_external_message",
                        "deliveryId": "d1",
                        "text": "hello",
                        "deliverAs": "steer",
                        "sender": {"kind": "agent", "session_id": "s1", "extra": 1}
                    }),
                    ""
                )
                .is_err()
        );
    }
}
