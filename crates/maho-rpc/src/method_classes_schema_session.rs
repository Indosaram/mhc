//! The schemas of the session-routed inbound records (senpi#4324).

use crate::method_classes_schema_delivery::delivery_record_schemas;
use crate::method_classes_schema_fields::{
    FieldSchema, RPC_INBOUND_LIMITS, Schema, bool_schema, id_schema, images_schema,
    literals_schema, model_entries_schema, num_schema, object_schema, opt, path_str_schema,
    pos_int_schema, req, retry_fallback_schema, secret_schema, session_schema, str_schema,
    strict_object_schema, strings_schema, thinking_level_schema, thread_settled_by_schema,
};

fn steer_or_follow_up(record_type: &'static str) -> Schema {
    session_schema(
        record_type,
        vec![
            req("message", str_schema()),
            opt("images", images_schema()),
            opt("enqueueOrder", num_schema()),
            opt("whenIdle", literals_schema(&["queue", "start"])),
            opt("clientMessageId", id_schema()),
            opt("clientTurnId", id_schema()),
            opt("expectedTurnId", id_schema()),
            opt("chainCount", pos_int_schema()),
        ],
    )
}

fn edit_message(record_type: &'static str) -> Schema {
    session_schema(
        record_type,
        vec![
            req("entryId", id_schema()),
            req("text", str_schema()),
            opt("expectedLeafId", id_schema()),
            opt("summarize", bool_schema()),
            opt("customInstructions", str_schema()),
        ],
    )
}

fn navigate_tree_fields(entry_first: bool) -> Vec<FieldSchema> {
    let mut fields = vec![
        opt("intent", literals_schema(&["select", "resume"])),
        opt("expectedLeafId", id_schema()),
        opt("summarize", bool_schema()),
        opt("customInstructions", str_schema()),
        opt("replaceInstructions", bool_schema()),
        opt("label", str_schema()),
    ];
    if entry_first {
        fields.push(req("entryId", id_schema()));
        fields.push(opt("targetId", Schema::Never));
    } else {
        fields.push(req("targetId", id_schema()));
        fields.push(opt("entryId", Schema::Never));
    }
    fields
}

pub fn session_record_schemas() -> Vec<(&'static str, Schema)> {
    let mut schemas = vec![
        // prompting
        (
            "prompt",
            session_schema(
                "prompt",
                vec![
                    req("message", str_schema()),
                    opt("images", images_schema()),
                    opt("streamingBehavior", literals_schema(&["steer", "followUp"])),
                    opt("thinkingLevel", thinking_level_schema()),
                    opt(
                        "sessionTitlePrompt",
                        Schema::Union(vec![str_schema(), Schema::LiteralBool(false)]),
                    ),
                    opt("expandPromptTemplates", bool_schema()),
                    opt("unknownCommandAsText", bool_schema()),
                    opt("clientMessageId", id_schema()),
                    opt("clientTurnId", id_schema()),
                    opt("expectedTurnId", id_schema()),
                    opt("expectedLeafId", id_schema()),
                    opt(
                        "toolBarrier",
                        object_schema(vec![
                            req("id", id_schema()),
                            req("deadlineMs", num_schema()),
                        ]),
                    ),
                    opt("chainCount", pos_int_schema()),
                ],
            ),
        ),
        (
            "release_tool_barrier",
            session_schema("release_tool_barrier", vec![req("barrierId", id_schema())]),
        ),
        (
            "send_custom_message",
            session_schema(
                "send_custom_message",
                vec![
                    req("customType", id_schema()),
                    req("content", Schema::Unknown),
                    req("display", bool_schema()),
                    opt("details", Schema::Unknown),
                    opt("triggerTurn", bool_schema()),
                    opt(
                        "deliverAs",
                        literals_schema(&["steer", "followUp", "nextTurn"]),
                    ),
                ],
            ),
        ),
        (
            "continue_from_leaf",
            session_schema("continue_from_leaf", vec![]),
        ),
        (
            "resume_session",
            session_schema(
                "resume_session",
                vec![
                    opt("expectedSeq", num_schema()),
                    opt("mode", literals_schema(&["continue", "release"])),
                    opt("message", str_schema()),
                    opt("effectId", id_schema()),
                    opt("unlessStopped", bool_schema()),
                ],
            ),
        ),
        (
            "append_user_message",
            session_schema("append_user_message", vec![req("content", Schema::Unknown)]),
        ),
        (
            "append_session_entry",
            session_schema("append_session_entry", vec![req("entry", Schema::Unknown)]),
        ),
        ("steer", steer_or_follow_up("steer")),
        ("follow_up", steer_or_follow_up("follow_up")),
        (
            "abort",
            session_schema(
                "abort",
                vec![
                    opt("effectId", id_schema()),
                    opt("awaitIdle", bool_schema()),
                    opt("clearQueue", bool_schema()),
                    opt("graceMs", num_schema()),
                    opt("intent", literals_schema(&["stop", "abort"])),
                ],
            ),
        ),
        (
            "interrupt",
            session_schema(
                "interrupt",
                vec![
                    opt("turnId", id_schema()),
                    opt("intent", literals_schema(&["stop", "abort"])),
                    opt("background", Schema::LiteralStr("stop")),
                ],
            ),
        ),
        (
            "hold_input",
            session_schema(
                "hold_input",
                vec![
                    req("inputId", id_schema()),
                    req("revision", num_schema()),
                    req("text", str_schema()),
                    opt("parentSessionId", id_schema()),
                    opt("origin", Schema::LiteralStr("human")),
                ],
            ),
        ),
        (
            "withdraw_held_input",
            session_schema("withdraw_held_input", vec![req("inputId", id_schema())]),
        ),
        (
            "pause_for_run",
            session_schema(
                "pause_for_run",
                vec![
                    req("parentSessionId", id_schema()),
                    req("parentPauseSeq", num_schema()),
                    req(
                        "runRef",
                        object_schema(vec![
                            req(
                                "kind",
                                literals_schema(&["dag", "task", "workpool", "team"]),
                            ),
                            req("id", id_schema()),
                        ]),
                    ),
                ],
            ),
        ),
        (
            "resume_for_run",
            session_schema(
                "resume_for_run",
                vec![
                    req("parentSessionId", id_schema()),
                    req("parentPauseSeq", num_schema()),
                    req("resumeId", id_schema()),
                    req("childSeqCutoff", num_schema()),
                ],
            ),
        ),
        (
            "abort_compaction",
            session_schema("abort_compaction", vec![]),
        ),
        ("reload", session_schema("reload", vec![])),
        (
            "check_reload_veto",
            session_schema("check_reload_veto", vec![]),
        ),
        (
            "clear_queue",
            session_schema("clear_queue", vec![opt("abortWillFollow", bool_schema())]),
        ),
        (
            "dequeue_message",
            session_schema(
                "dequeue_message",
                vec![
                    req("queuedId", id_schema()),
                    opt("expectedRevision", num_schema()),
                    opt("effectId", id_schema()),
                ],
            ),
        ),
        (
            "edit_queued_message",
            session_schema(
                "edit_queued_message",
                vec![
                    req("queuedId", id_schema()),
                    req("text", str_schema()),
                    opt("expectedRevision", num_schema()),
                    opt("effectId", id_schema()),
                ],
            ),
        ),
        (
            "get_steering_messages",
            session_schema("get_steering_messages", vec![]),
        ),
        (
            "get_follow_up_messages",
            session_schema("get_follow_up_messages", vec![]),
        ),
        (
            "abort_branch_summary",
            session_schema("abort_branch_summary", vec![]),
        ),
        (
            "new_session",
            session_schema("new_session", vec![opt("parentSession", path_str_schema())]),
        ),
        (
            "wake",
            session_schema("wake", vec![opt("delivery_ids", strings_schema())]),
        ),
    ];
    schemas.extend(delivery_record_schemas());
    schemas.extend([
        // state and model
        ("get_state", session_schema("get_state", vec![])),
        (
            "get_snapshot",
            session_schema(
                "get_snapshot",
                vec![
                    opt("tail", num_schema()),
                    opt("maxBytes", num_schema()),
                    opt("observe", bool_schema()),
                ],
            ),
        ),
        (
            "request_self_settle",
            session_schema("request_self_settle", vec![]),
        ),
        (
            "get_effect_status",
            session_schema(
                "get_effect_status",
                vec![
                    req("effectId", id_schema()),
                    opt("sessionPath", path_str_schema()),
                ],
            ),
        ),
        (
            "get_pending_inputs",
            session_schema(
                "get_pending_inputs",
                vec![opt("knownClientMessageIds", strings_schema())],
            ),
        ),
        (
            "set_model",
            session_schema(
                "set_model",
                vec![
                    req("provider", id_schema()),
                    req("modelId", id_schema()),
                    opt("scope", Schema::LiteralStr("session")),
                    opt("effectId", id_schema()),
                ],
            ),
        ),
        (
            "set_favorite_models",
            session_schema(
                "set_favorite_models",
                vec![req("models", model_entries_schema())],
            ),
        ),
        (
            "set_scoped_models",
            session_schema(
                "set_scoped_models",
                vec![req("models", model_entries_schema())],
            ),
        ),
        (
            "cycle_model",
            session_schema(
                "cycle_model",
                vec![opt("direction", literals_schema(&["forward", "backward"]))],
            ),
        ),
        (
            "get_available_models",
            session_schema("get_available_models", vec![]),
        ),
        (
            "set_thinking_level",
            session_schema(
                "set_thinking_level",
                vec![
                    req("level", thinking_level_schema()),
                    opt("scope", Schema::LiteralStr("turn")),
                    opt("effectId", id_schema()),
                ],
            ),
        ),
        (
            "cycle_thinking_level",
            session_schema("cycle_thinking_level", vec![]),
        ),
        (
            "get_available_thinking_levels",
            session_schema("get_available_thinking_levels", vec![]),
        ),
        (
            "get_model_thinking_level",
            session_schema(
                "get_model_thinking_level",
                vec![req("provider", id_schema()), req("modelId", id_schema())],
            ),
        ),
        (
            "set_fast_mode",
            session_schema("set_fast_mode", vec![req("enabled", bool_schema())]),
        ),
        ("get_fast_mode", session_schema("get_fast_mode", vec![])),
        (
            "set_steering_mode",
            session_schema(
                "set_steering_mode",
                vec![req("mode", literals_schema(&["all", "one-at-a-time"]))],
            ),
        ),
        (
            "set_follow_up_mode",
            session_schema(
                "set_follow_up_mode",
                vec![req("mode", literals_schema(&["all", "one-at-a-time"]))],
            ),
        ),
        (
            "compact",
            session_schema("compact", vec![opt("customInstructions", str_schema())]),
        ),
        (
            "set_auto_compaction",
            session_schema("set_auto_compaction", vec![req("enabled", bool_schema())]),
        ),
        (
            "set_permission_preset",
            session_schema(
                "set_permission_preset",
                vec![req("permissionPreset", id_schema())],
            ),
        ),
        (
            "set_auto_retry",
            session_schema("set_auto_retry", vec![req("enabled", bool_schema())]),
        ),
        ("abort_retry", session_schema("abort_retry", vec![])),
        (
            "set_retry_fallback",
            session_schema(
                "set_retry_fallback",
                vec![req("retryFallback", retry_fallback_schema())],
            ),
        ),
        // bash and tree
        (
            "bash",
            session_schema(
                "bash",
                vec![
                    req("command", str_schema()),
                    opt("bashId", id_schema()),
                    opt("excludeFromContext", bool_schema()),
                    opt("executionId", id_schema()),
                    opt(
                        "operations",
                        Schema::Record {
                            key_env_name: false,
                            key_max_len: Some(RPC_INBOUND_LIMITS.identifier_characters),
                            value: Box::new(Schema::Unknown),
                            max_properties: Some(RPC_INBOUND_LIMITS.record_keys),
                            additional_properties: true,
                        },
                    ),
                ],
            ),
        ),
        (
            "record_bash_result",
            session_schema(
                "record_bash_result",
                vec![
                    req("command", str_schema()),
                    req(
                        "result",
                        object_schema(vec![
                            req("output", str_schema()),
                            opt(
                                "exitCode",
                                Schema::Integer {
                                    min: None,
                                    max: None,
                                },
                            ),
                            req("cancelled", bool_schema()),
                            req("truncated", bool_schema()),
                            opt("fullOutputPath", path_str_schema()),
                        ]),
                    ),
                    opt("excludeFromContext", bool_schema()),
                ],
            ),
        ),
        ("abort_bash", session_schema("abort_bash", vec![])),
        (
            "cleanup_bash_output",
            session_schema("cleanup_bash_output", vec![req("path", path_str_schema())]),
        ),
        (
            "set_label",
            session_schema(
                "set_label",
                vec![req("entryId", id_schema()), opt("label", str_schema())],
            ),
        ),
        (
            "navigate_tree",
            Schema::Union(vec![
                session_schema("navigate_tree", navigate_tree_fields(true)),
                session_schema("navigate_tree", navigate_tree_fields(false)),
            ]),
        ),
        (
            "get_session_stats",
            session_schema("get_session_stats", vec![]),
        ),
        (
            "resolve_path",
            session_schema("resolve_path", vec![req("path", path_str_schema())]),
        ),
        ("memory_report", session_schema("memory_report", vec![])),
        (
            "export_html",
            session_schema(
                "export_html",
                vec![
                    opt("outputPath", path_str_schema()),
                    opt("themeName", id_schema()),
                ],
            ),
        ),
        (
            "export_jsonl",
            session_schema("export_jsonl", vec![opt("outputPath", path_str_schema())]),
        ),
        (
            "switch_session",
            session_schema(
                "switch_session",
                vec![
                    req("sessionPath", path_str_schema()),
                    opt("cwdOverride", path_str_schema()),
                ],
            ),
        ),
        (
            "fork",
            session_schema(
                "fork",
                vec![
                    req("entryId", id_schema()),
                    opt("position", literals_schema(&["before", "at"])),
                ],
            ),
        ),
        (
            "edit_assistant_message",
            edit_message("edit_assistant_message"),
        ),
        ("edit_user_message", edit_message("edit_user_message")),
        ("clone", session_schema("clone", vec![])),
        (
            "get_fork_messages",
            session_schema("get_fork_messages", vec![]),
        ),
        (
            "get_entries",
            session_schema(
                "get_entries",
                vec![
                    opt("since", id_schema()),
                    opt("before", id_schema()),
                    opt("limit", num_schema()),
                    opt("tail", num_schema()),
                    opt("maxBytes", num_schema()),
                ],
            ),
        ),
        ("get_tree", session_schema("get_tree", vec![])),
        (
            "get_last_assistant_text",
            session_schema("get_last_assistant_text", vec![]),
        ),
        (
            "set_session_name",
            session_schema("set_session_name", vec![req("name", str_schema())]),
        ),
        (
            "set_session_archived",
            session_schema("set_session_archived", vec![req("archived", bool_schema())]),
        ),
        (
            "set_session_settled",
            session_schema(
                "set_session_settled",
                vec![
                    req("settled", bool_schema()),
                    opt("by", thread_settled_by_schema()),
                ],
            ),
        ),
        (
            "record_user_activity",
            session_schema(
                "record_user_activity",
                vec![req("kind", Schema::LiteralStr("draft"))],
            ),
        ),
        (
            "import_jsonl",
            session_schema(
                "import_jsonl",
                vec![
                    req("inputPath", path_str_schema()),
                    opt("cwdOverride", path_str_schema()),
                ],
            ),
        ),
        ("get_messages", session_schema("get_messages", vec![])),
        (
            "get_media",
            session_schema(
                "get_media",
                vec![
                    req("toolCallId", id_schema()),
                    req("contentIndex", num_schema()),
                ],
            ),
        ),
        // surface and account
        ("get_commands", session_schema("get_commands", vec![])),
        (
            "get_loaded_surfaces",
            session_schema("get_loaded_surfaces", vec![]),
        ),
        (
            "get_project_trust",
            session_schema("get_project_trust", vec![]),
        ),
        (
            "set_project_trust",
            session_schema(
                "set_project_trust",
                vec![
                    req(
                        "choice",
                        literals_schema(&["trust", "trust_parent", "do_not_trust", "no_inherit"]),
                    ),
                    opt("expectedCwd", str_schema()),
                ],
            ),
        ),
        (
            "get_project_settings",
            session_schema("get_project_settings", vec![]),
        ),
        (
            "set_project_setting",
            session_schema(
                "set_project_setting",
                vec![
                    req("key", id_schema()),
                    req("value", Schema::Unknown),
                    opt("expectedCwd", str_schema()),
                ],
            ),
        ),
        (
            "read_project_file",
            session_schema("read_project_file", vec![req("path", path_str_schema())]),
        ),
        (
            "write_project_file",
            session_schema(
                "write_project_file",
                vec![
                    req("path", path_str_schema()),
                    req("content", str_schema()),
                    req("ifEtag", Schema::Union(vec![id_schema(), Schema::Null])),
                    opt("expectedCwd", str_schema()),
                ],
            ),
        ),
        (
            "get_project_mcp_servers",
            session_schema("get_project_mcp_servers", vec![]),
        ),
        (
            "set_project_mcp_servers",
            session_schema(
                "set_project_mcp_servers",
                vec![
                    req("patch", Schema::Unknown),
                    req("ifEtag", Schema::Union(vec![id_schema(), Schema::Null])),
                    opt("expectedCwd", str_schema()),
                ],
            ),
        ),
        (
            "read_project_skill_file",
            session_schema("read_project_skill_file", vec![req("skill", id_schema())]),
        ),
        (
            "write_project_skill_file",
            session_schema(
                "write_project_skill_file",
                vec![
                    req("skill", id_schema()),
                    req("content", str_schema()),
                    req("ifEtag", Schema::Union(vec![id_schema(), Schema::Null])),
                    opt("expectedCwd", str_schema()),
                ],
            ),
        ),
        (
            "read_user_skill_file",
            session_schema("read_user_skill_file", vec![req("skill", id_schema())]),
        ),
        (
            "write_user_skill_file",
            session_schema(
                "write_user_skill_file",
                vec![
                    req("skill", id_schema()),
                    req("content", str_schema()),
                    req("ifEtag", Schema::Union(vec![id_schema(), Schema::Null])),
                ],
            ),
        ),
        (
            "extension_request",
            session_schema(
                "extension_request",
                vec![req("name", id_schema()), opt("data", Schema::Unknown)],
            ),
        ),
        ("eval_cells.list", session_schema("eval_cells.list", vec![])),
        (
            "eval_cells.peek",
            session_schema(
                "eval_cells.peek",
                vec![req("cellId", id_schema()), opt("tailBytes", num_schema())],
            ),
        ),
        (
            "eval_cells.stop",
            session_schema("eval_cells.stop", vec![req("cellId", id_schema())]),
        ),
        (
            "get_auth_providers",
            session_schema("get_auth_providers", vec![]),
        ),
        (
            "login_start",
            session_schema(
                "login_start",
                vec![req("provider", id_schema()), opt("slotId", id_schema())],
            ),
        ),
        (
            "login_cancel",
            session_schema("login_cancel", vec![req("provider", id_schema())]),
        ),
        (
            "login_api_key",
            session_schema(
                "login_api_key",
                vec![
                    req("provider", id_schema()),
                    req("key", secret_schema()),
                    opt("region", id_schema()),
                ],
            ),
        ),
        (
            "logout",
            session_schema(
                "logout",
                vec![req("provider", id_schema()), opt("slotId", id_schema())],
            ),
        ),
        (
            "verify_api_key",
            session_schema(
                "verify_api_key",
                vec![req("provider", id_schema()), req("key", secret_schema())],
            ),
        ),
        (
            "get_provider_accounts",
            session_schema("get_provider_accounts", vec![req("provider", id_schema())]),
        ),
        (
            "account_pin",
            session_schema(
                "account_pin",
                vec![
                    req("provider", id_schema()),
                    req("name", Schema::Union(vec![id_schema(), Schema::Null])),
                ],
            ),
        ),
        (
            "account_remove",
            session_schema(
                "account_remove",
                vec![req("provider", id_schema()), req("name", id_schema())],
            ),
        ),
        (
            "account_reset",
            session_schema(
                "account_reset",
                vec![req("provider", id_schema()), req("name", id_schema())],
            ),
        ),
        (
            "account_order",
            session_schema(
                "account_order",
                vec![
                    req("provider", id_schema()),
                    req("names", Schema::Union(vec![strings_schema(), Schema::Null])),
                ],
            ),
        ),
        (
            "test_provider_account",
            session_schema(
                "test_provider_account",
                vec![req("provider", id_schema()), req("name", id_schema())],
            ),
        ),
        (
            "set_client_info",
            session_schema(
                "set_client_info",
                vec![
                    req(
                        "width",
                        Schema::Integer {
                            min: Some(1),
                            max: Some(100_000),
                        },
                    ),
                    opt("capabilities", strings_schema()),
                    opt(
                        "remoteOrigin",
                        strict_object_schema(vec![
                            req("deviceId", id_schema()),
                            opt("deviceName", id_schema()),
                        ]),
                    ),
                ],
            ),
        ),
    ]);
    schemas
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn navigate_tree_requires_exactly_one_target_id_or_entry_id() {
        let schemas = session_record_schemas();
        let nav = &schemas
            .iter()
            .find(|(k, _)| *k == "navigate_tree")
            .expect("nav")
            .1;
        assert!(
            nav.validate(&json!({"type": "navigate_tree", "entryId": "e1"}), "")
                .is_ok()
        );
        assert!(
            nav.validate(&json!({"type": "navigate_tree", "targetId": "t1"}), "")
                .is_ok()
        );
        assert!(
            nav.validate(
                &json!({"type": "navigate_tree", "entryId": "e1", "targetId": "t1"}),
                ""
            )
            .is_err()
        );
        assert!(nav.validate(&json!({"type": "navigate_tree"}), "").is_err());
    }
}
