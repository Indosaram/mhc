//! The schemas of the host-level inbound records and of the answers a client sends back (senpi#4324).

use crate::method_classes_schema_fields::{
    FieldSchema, RPC_INBOUND_LIMITS, Schema, bool_schema, command_schema, id_schema,
    literals_schema, num_schema, object_schema, opt, path_str_schema, pos_int_schema,
    question_answers_schema, req, retry_fallback_schema, session_context_schema,
    session_env_schema, session_kind_schema, str_schema, strings_schema, thinking_level_schema,
};

pub const MOVE_INLINE_MAX_BYTES: usize = 1024 * 1024;
pub const MOVE_INLINE_MAX_FILES: usize = 1024;

fn move_cmd(properties: Vec<FieldSchema>) -> Schema {
    command_schema("move_session", properties)
}

fn repository_identity_schema() -> Schema {
    object_schema(vec![
        req("rootCommits", strings_schema()),
        opt("originUrl", path_str_schema()),
    ])
}

fn ui_response_frame_fields() -> Vec<FieldSchema> {
    vec![
        opt("secret", bool_schema()),
        req("type", Schema::LiteralStr("extension_ui_response")),
        req("id", id_schema()),
        opt("uiRequestId", id_schema()),
        opt("answerId", id_schema()),
    ]
}

fn open_shared_tail_properties() -> Vec<FieldSchema> {
    vec![
        opt("provider", id_schema()),
        opt("modelId", id_schema()),
        opt("thinkingLevel", thinking_level_schema()),
        opt("permissionPreset", id_schema()),
        opt("retain_on_disconnect", bool_schema()),
        opt("kind", session_kind_schema()),
        opt("context", session_context_schema()),
        opt("auto_title", bool_schema()),
        opt("durableSessionId", id_schema()),
        opt(
            "promptSurface",
            literals_schema(&["terminal", "app", "chat"]),
        ),
        opt(
            "browserEngine",
            literals_schema(&["connected", "builtin", "none"]),
        ),
        opt("retryFallback", retry_fallback_schema()),
        opt("env", session_env_schema()),
    ]
}

fn open_prefix_properties(
    include_session_path: bool,
    cwd_required: Option<bool>,
) -> Vec<FieldSchema> {
    let mut fields = vec![
        opt("retry", Schema::LiteralBool(true)),
        opt("attended", Schema::LiteralBool(false)),
        opt(
            "spawnedBy",
            object_schema(vec![
                req("parentSessionId", id_schema()),
                req("rootSessionId", id_schema()),
                req("taskId", id_schema()),
            ]),
        ),
        opt("expectedHostInstance", id_schema()),
    ];
    if include_session_path {
        fields.push(opt("sessionPath", path_str_schema()));
    }
    match cwd_required {
        Some(true) => fields.push(req("cwd", path_str_schema())),
        Some(false) => fields.push(opt("cwd", path_str_schema())),
        None => {}
    }
    fields.extend(open_shared_tail_properties());
    fields
}

fn move_inline_file_schema() -> Schema {
    let max_b64 = MOVE_INLINE_MAX_BYTES.div_ceil(3) * 4;
    object_schema(vec![
        req("path", path_str_schema()),
        req("sha256", id_schema()),
        req(
            "base64",
            Schema::String {
                max_len: Some(max_b64),
                env_name: false,
            },
        ),
    ])
}

pub fn host_record_schemas() -> Vec<(&'static str, Schema)> {
    let ui_frame_with = |extra: FieldSchema| {
        let mut fields = ui_response_frame_fields();
        fields.push(extra);
        object_schema(fields)
    };
    let mut answers_fields = ui_response_frame_fields();
    answers_fields.push(req("answers", question_answers_schema()));
    answers_fields.push(opt("comment", str_schema()));

    vec![
        (
            "get_protocol_info",
            command_schema(
                "get_protocol_info",
                vec![
                    opt(
                        "auth",
                        object_schema(vec![req("client_nonce", id_schema())]),
                    ),
                    opt("observe", bool_schema()),
                ],
            ),
        ),
        (
            "open_session",
            command_schema("open_session", open_prefix_properties(true, Some(false))),
        ),
        (
            "claim_session",
            command_schema("claim_session", open_prefix_properties(true, Some(true))),
        ),
        (
            "park_session",
            command_schema(
                "park_session",
                vec![
                    req("cwd", path_str_schema()),
                    opt("kind", session_kind_schema()),
                    opt("context", session_context_schema()),
                ],
            ),
        ),
        (
            "close_session",
            command_schema("close_session", vec![req("sessionId", id_schema())]),
        ),
        (
            "move_session",
            Schema::Union(vec![
                move_cmd(vec![
                    req("op", Schema::LiteralStr("prepare")),
                    req("moveId", id_schema()),
                    opt("cwd", path_str_schema()),
                    opt("repository", repository_identity_schema()),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("cancel")),
                    req("moveId", id_schema()),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("identity")),
                    req("sessionPath", path_str_schema()),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("identity")),
                    req("cwd", path_str_schema()),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("status")),
                    req("moveId", id_schema()),
                    opt("sessionPath", path_str_schema()),
                    opt(
                        "landing",
                        object_schema(vec![
                            req("cwd", path_str_schema()),
                            req("sessionFileName", id_schema()),
                            opt("sessionDir", path_str_schema()),
                        ]),
                    ),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("export")),
                    req("sessionPath", path_str_schema()),
                    req("moveId", id_schema()),
                    req("target", id_schema()),
                    opt("droppedUserMessages", strings_schema()),
                    opt("inline", Schema::LiteralBool(true)),
                    opt("inlineMaxBytes", num_schema()),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("import")),
                    req("cwd", path_str_schema()),
                    opt("from", id_schema()),
                    opt("bundleDir", path_str_schema()),
                    opt(
                        "files",
                        Schema::Array {
                            item: Box::new(move_inline_file_schema()),
                            max_items: Some(MOVE_INLINE_MAX_FILES),
                        },
                    ),
                    opt("sessionDir", path_str_schema()),
                    opt(
                        "open",
                        Schema::Union(vec![
                            Schema::LiteralBool(true),
                            object_schema(open_prefix_properties(false, None)),
                        ]),
                    ),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("finish")),
                    req("sessionPath", path_str_schema()),
                    req("moveId", id_schema()),
                    req("answer", Schema::Unknown),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("revoke")),
                    req("sessionPath", path_str_schema()),
                    req("moveId", id_schema()),
                ]),
                move_cmd(vec![
                    req("op", Schema::LiteralStr("copy")),
                    req("sessionPath", path_str_schema()),
                    req("sessionId", id_schema()),
                    opt("sessionDir", path_str_schema()),
                ]),
            ]),
        ),
        (
            "release_session",
            command_schema(
                "release_session",
                vec![
                    req("sessionId", id_schema()),
                    req("reason", literals_schema(&["takeover", "move"])),
                    opt("moveId", id_schema()),
                    opt("target", id_schema()),
                    opt("interrupt", bool_schema()),
                    opt("force", bool_schema()),
                ],
            ),
        ),
        (
            "unload_session",
            command_schema(
                "unload_session",
                vec![
                    req("sessionId", id_schema()),
                    opt("effectId", id_schema()),
                    opt("interrupt", bool_schema()),
                    opt("clearQueue", bool_schema()),
                    opt("children", literals_schema(&["refuse", "detach"])),
                    opt("force", bool_schema()),
                    opt("deliveries", Schema::LiteralStr("refuse")),
                    opt("graceful", bool_schema()),
                    opt("waitMs", pos_int_schema()),
                ],
            ),
        ),
        (
            "detach_connection",
            command_schema("detach_connection", vec![]),
        ),
        (
            "find_session",
            command_schema("find_session", vec![req("durableSessionId", id_schema())]),
        ),
        (
            "get_subscription_limits",
            command_schema(
                "get_subscription_limits",
                vec![opt("observe", bool_schema())],
            ),
        ),
        (
            "list_sessions",
            command_schema(
                "list_sessions",
                vec![
                    opt("include_workers", bool_schema()),
                    opt("observe", bool_schema()),
                ],
            ),
        ),
        (
            "subscribe_sessions",
            command_schema(
                "subscribe_sessions",
                vec![
                    opt("retry", Schema::LiteralBool(true)),
                    opt(
                        "sessions",
                        Schema::Union(vec![
                            Schema::LiteralStr("all"),
                            object_schema(vec![req("ids", strings_schema())]),
                            object_schema(vec![req("childrenOf", id_schema())]),
                        ]),
                    ),
                    opt("include_workers", bool_schema()),
                    opt("include_context", bool_schema()),
                    opt("effects", bool_schema()),
                    opt("dialogs", bool_schema()),
                    opt(
                        "mirror",
                        Schema::Union(vec![
                            Schema::LiteralStr("none"),
                            Schema::LiteralStr("all"),
                            strings_schema(),
                        ]),
                    ),
                    opt("observe", bool_schema()),
                ],
            ),
        ),
        (
            "unsubscribe_sessions",
            command_schema(
                "unsubscribe_sessions",
                vec![
                    req("subscriptionId", id_schema()),
                    opt("observe", bool_schema()),
                ],
            ),
        ),
        (
            "set_session_context",
            command_schema(
                "set_session_context",
                vec![
                    req("sessionId", id_schema()),
                    req(
                        "patch",
                        Schema::Record {
                            key_env_name: false,
                            key_max_len: Some(RPC_INBOUND_LIMITS.identifier_characters),
                            value: Box::new(Schema::Union(vec![
                                str_schema(),
                                Schema::Number,
                                bool_schema(),
                                Schema::Null,
                            ])),
                            max_properties: Some(RPC_INBOUND_LIMITS.record_keys),
                            additional_properties: true,
                        },
                    ),
                ],
            ),
        ),
        (
            "warm",
            command_schema(
                "warm",
                vec![
                    opt("cwd", path_str_schema()),
                    opt("kind", session_kind_schema()),
                    opt("context", session_context_schema()),
                ],
            ),
        ),
        (
            "attach_session",
            command_schema(
                "attach_session",
                vec![
                    req("sessionId", id_schema()),
                    req("durableSessionId", id_schema()),
                    opt("expectedHostInstance", id_schema()),
                    opt("retry", Schema::LiteralBool(true)),
                    opt("takeover", Schema::LiteralBool(true)),
                    opt("holder", id_schema()),
                ],
            ),
        ),
        (
            "release_attachment",
            command_schema("release_attachment", vec![req("sessionId", id_schema())]),
        ),
        (
            "list_hands",
            command_schema("list_hands", vec![opt("sessionId", id_schema())]),
        ),
        (
            "select_hand",
            command_schema(
                "select_hand",
                vec![
                    req("sessionId", id_schema()),
                    req("device", Schema::Union(vec![id_schema(), Schema::Null])),
                ],
            ),
        ),
        (
            "reclaim_session",
            command_schema(
                "reclaim_session",
                vec![
                    req("sessionPath", path_str_schema()),
                    req("durableSessionId", id_schema()),
                    req("expectedHostInstance", id_schema()),
                ],
            ),
        ),
        (
            "extension_ui_response",
            Schema::Union(vec![
                ui_frame_with(req("value", str_schema())),
                ui_frame_with(req("confirmed", bool_schema())),
                ui_frame_with(req("cancelled", Schema::LiteralBool(true))),
                object_schema(answers_fields),
            ]),
        ),
        (
            "extension_ui_progress",
            object_schema(vec![
                req("type", Schema::LiteralStr("extension_ui_progress")),
                req("id", id_schema()),
                opt("answers", question_answers_schema()),
                opt("comment", str_schema()),
                opt("sessionId", id_schema()),
            ]),
        ),
        (
            "host_response",
            object_schema(vec![
                req("type", Schema::LiteralStr("host_response")),
                req("id", id_schema()),
                opt("sessionId", id_schema()),
                opt("data", Schema::Unknown),
                opt("error", str_schema()),
            ]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn move_session_validates_op_variants_and_open_session_validates_env() {
        let schemas = host_record_schemas();
        let move_schema = &schemas
            .iter()
            .find(|(k, _)| *k == "move_session")
            .expect("move")
            .1;
        assert!(
            move_schema
                .validate(
                    &json!({"type": "move_session", "op": "cancel", "moveId": "m1"}),
                    ""
                )
                .is_ok()
        );
        assert!(
            move_schema
                .validate(&json!({"type": "move_session", "op": "prepare"}), "")
                .is_err()
        );
        let open_schema = &schemas
            .iter()
            .find(|(k, _)| *k == "open_session")
            .expect("open")
            .1;
        assert!(
            open_schema
                .validate(
                    &json!({"type": "open_session", "env": {"OMOCAT_LANE": "l1"}}),
                    ""
                )
                .is_ok()
        );
        assert!(
            open_schema
                .validate(&json!({"type": "open_session", "env": {"bad": "l1"}}), "")
                .is_err()
        );
    }
}
