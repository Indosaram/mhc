use omo_config_core::internal::validate::{Node, safe_parse};
use omo_config_core::issue::Issue;
use serde_json::{Value, json};

fn parsed(node: &Node, value: Value) -> Value {
    match safe_parse(node, &value) {
        Ok(parsed) => parsed,
        Err(issues) => panic!("expected a valid document, got {issues:?}"),
    }
}

fn issues(node: &Node, value: Value) -> Vec<Issue> {
    match safe_parse(node, &value) {
        Ok(parsed) => panic!("expected validation to fail, got {parsed}"),
        Err(issues) => issues,
    }
}

fn issue_paths(issues: &[Issue]) -> Vec<String> {
    issues.iter().map(|issue| issue.path_string()).collect()
}

#[test]
fn model_ref_accepts_canonical_levels_auto_and_a_native_preset() {
    let node = omo_config_core::omo_reasoning_schema();
    for value in [
        "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto", "thinking",
    ] {
        assert!(
            safe_parse(&node, &json!(value)).is_ok(),
            "expected {value} to be accepted"
        );
    }
}

#[test]
fn model_ref_keeps_string_and_object_forms() {
    let string_ref = "openai/gpt-5:high";
    let object_ref = json!({
        "model": "openai/gpt-5",
        "reasoning": "high",
        "temperature": 0.4,
        "top_p": 0.8,
        "max_tokens": 4096,
        "provider_options": { "service_tier": "priority" },
    });
    assert_eq!(
        parsed(&omo_config_core::omo_model_ref_schema(), json!(string_ref)),
        json!(string_ref)
    );
    assert_eq!(
        parsed(&omo_config_core::omo_model_ref_schema(), object_ref.clone()),
        object_ref
    );
}

#[test]
fn model_ref_object_rejects_out_of_range_tuning_and_unknown_keys() {
    let node = omo_config_core::omo_model_ref_object_schema();
    for value in [
        json!({ "model": "openai/gpt-5", "temperature": 2.1 }),
        json!({ "model": "openai/gpt-5", "top_p": -0.1 }),
        json!({ "model": "openai/gpt-5", "max_tokens": 0 }),
        json!({ "model": "openai/gpt-5", "variant": "high" }),
    ] {
        assert!(
            safe_parse(&node, &value).is_err(),
            "expected {value} to be rejected"
        );
    }
}

#[test]
fn telemetry_harness_support_names_senpi_for_the_enabled_setting() {
    assert_eq!(
        omo_config_core::TELEMETRY_HARNESS_SUPPORT[0].0,
        "telemetry.enabled"
    );
    assert!(
        omo_config_core::TELEMETRY_HARNESS_SUPPORT[0]
            .1
            .contains(&"senpi")
    );
}

#[test]
fn category_keeps_warn_unavailable_absent_when_not_declared() {
    let node = omo_config_core::omo_category_config_schema();
    let value = parsed(&node, json!({}));
    assert!(value.get("warn_unavailable").is_none());
}

#[test]
fn category_preserves_a_boolean_warn_unavailable_override() {
    let node = omo_config_core::omo_category_config_schema();
    let value = parsed(&node, json!({ "warn_unavailable": true }));
    assert_eq!(value["warn_unavailable"], json!(true));
}

#[test]
fn category_rejects_a_non_boolean_warn_unavailable_at_the_field_path() {
    let node = omo_config_core::omo_category_config_schema();
    let reported = issues(&node, json!({ "warn_unavailable": "nope" }));
    assert!(
        issue_paths(&reported)
            .join(",")
            .contains("warn_unavailable")
    );
}

#[test]
fn normalize_legacy_model_entry_canonicalizes_every_deprecated_reasoning_spelling() {
    let entries = [
        json!({ "model": "openai/gpt-5", "variant": "high" }),
        json!({ "model": "openai/gpt-5", "reasoningEffort": "none" }),
        json!({ "model": "openai/gpt-5", "variant": "low", "reasoningEffort": "xhigh" }),
        json!({ "model": "openai/gpt-5", "thinking": { "type": "disabled" } }),
    ];
    let normalized: Vec<Value> = entries
        .iter()
        .map(|entry| omo_config_core::normalize_legacy_model_entry(entry).expect("entry"))
        .collect();
    let reasonings: Vec<&str> = normalized
        .iter()
        .map(|entry| entry["reasoning"].as_str().expect("reasoning"))
        .collect();
    assert_eq!(reasonings, vec!["high", "off", "xhigh", "off"]);
}

#[test]
fn normalize_legacy_model_entry_moves_wire_settings_under_provider_options() {
    let thinking = json!({ "type": "enabled", "budgetTokens": 4096 });
    let normalized = omo_config_core::normalize_legacy_model_entry(&json!({
        "model": "anthropic/claude",
        "thinking": thinking,
        "textVerbosity": "low",
    }))
    .expect("entry");
    assert_eq!(
        normalized["provider_options"],
        json!({ "thinking": { "type": "enabled", "budgetTokens": 4096 }, "textVerbosity": "low" })
    );
}

#[test]
fn normalize_legacy_model_entry_canonicalizes_casing_and_legacy_suffixes() {
    let entries = [
        json!({ "model": "openai/gpt-5(xhigh)", "maxTokens": 8192 }),
        json!({ "model": "openai/gpt-5 minimal" }),
        json!({ "model": "openai/gpt-5:high" }),
    ];
    let normalized: Vec<Value> = entries
        .iter()
        .map(|entry| omo_config_core::normalize_legacy_model_entry(entry).expect("entry"))
        .collect();
    assert_eq!(
        normalized,
        vec![
            json!({ "model": "openai/gpt-5:xhigh", "max_tokens": 8192 }),
            json!({ "model": "openai/gpt-5:minimal" }),
            json!({ "model": "openai/gpt-5:high" }),
        ]
    );
}

#[test]
fn normalize_legacy_model_entry_merges_alias_options_without_discarding_canonical_values() {
    let normalized = omo_config_core::normalize_legacy_model_entry(&json!({
        "model": "openai/gpt-5",
        "provider_options": { "service_tier": "priority" },
        "textVerbosity": "high",
    }))
    .expect("entry");
    assert_eq!(
        normalized["provider_options"],
        json!({ "service_tier": "priority", "textVerbosity": "high" })
    );
}

#[test]
fn agent_model_entries_turn_per_entry_effort_into_canonical_reasoning() {
    let node = omo_config_core::omo_agent_def_schema();
    let value = parsed(
        &node,
        json!({
            "models": [
                { "model": "quotio-openai/gpt-5.6-luna-fast", "reasoningEffort": "minimal" },
                { "model": "quotio-openai/gpt-5.6-luna-fast", "reasoningEffort": "minimal" },
            ]
        }),
    );
    let entry = &value["models"][0];
    assert_eq!(entry["model"], json!("quotio-openai/gpt-5.6-luna-fast"));
    assert_eq!(entry["reasoning"], json!("minimal"));
}

#[test]
fn agent_model_entries_accept_mixed_strings_and_objects() {
    let node = omo_config_core::omo_agent_def_schema();
    let value = parsed(
        &node,
        json!({
            "model": "kimi-coding/kimi-for-coding-highspeed",
            "models": [
                "quotio-openai/gpt-5.6-luna-fast",
                { "model": "anthropic/claude-haiku-4-5", "variant": "low" },
            ]
        }),
    );
    assert_eq!(value["models"][0], json!("quotio-openai/gpt-5.6-luna-fast"));
    assert_eq!(value["models"][1]["reasoning"], json!("low"));
}

#[test]
fn agent_top_level_variant_and_effort_resolve_with_effort_winning() {
    let node = omo_config_core::omo_agent_def_schema();
    let value = parsed(
        &node,
        json!({
            "model": "quotio-openai/gpt-5.6-luna-fast",
            "variant": "low",
            "reasoningEffort": "minimal",
        }),
    );
    assert_eq!(value["reasoning"], json!("minimal"));
    assert!(value.get("variant").is_none());
    assert!(value.get("reasoningEffort").is_none());
}

#[test]
fn agent_plain_string_models_still_parse_unchanged() {
    let node = omo_config_core::omo_agent_def_schema();
    let value = parsed(
        &node,
        json!({ "models": ["anthropic/claude", "openai/gpt-5"] }),
    );
    assert_eq!(value["models"], json!(["anthropic/claude", "openai/gpt-5"]));
}

#[test]
fn agent_harness_native_reasoning_preset_passes_through() {
    let node = omo_config_core::omo_agent_def_schema();
    let value = parsed(
        &node,
        json!({ "models": [{ "model": "openai/gpt-5", "reasoningEffort": "turbo" }] }),
    );
    assert_eq!(value["models"][0]["reasoning"], json!("turbo"));
}

#[test]
fn agent_model_entry_without_a_model_is_rejected() {
    let node = omo_config_core::omo_agent_def_schema();
    assert!(
        safe_parse(
            &node,
            &json!({ "models": [{ "reasoningEffort": "minimal" }] })
        )
        .is_err()
    );
}

#[test]
fn agent_model_entry_with_an_unknown_key_is_rejected() {
    let node = omo_config_core::omo_agent_def_schema();
    assert!(
        safe_parse(
            &node,
            &json!({ "models": [{ "model": "openai/gpt-5", "effort": "minimal" }] })
        )
        .is_err()
    );
}

#[test]
fn config_schema_normalizes_defaults_and_deprecated_category_keys() {
    let config = json!({
        "$schema": "https://example.com/omo.schema.json",
        "categories": {
            "deep": {
                "description": "Deep analysis",
                "model": "anthropic/claude",
                "fallback_models": ["openai/gpt"],
                "variant": "high",
                "temperature": 0.2,
                "top_p": 0.9,
                "maxTokens": 12000,
                "thinking": { "type": "enabled", "budgetTokens": 2048 },
                "reasoningEffort": "high",
                "textVerbosity": "medium",
                "tools": { "bash": true },
                "prompt_append": "Think carefully.",
                "max_prompt_tokens": 2000,
                "is_unstable_agent": false,
                "disable": false,
            }
        },
        "agents": {
            "reviewer": {
                "description": "Reviews code",
                "prompt": "Review this.",
                "model": "openai/gpt-5",
                "models": ["anthropic/claude"],
                "tools": { "bash": false, "read": true },
                "execution_mode": "in-process",
                "background": true,
                "max_depth": 1,
                "allowed_subagents": ["quick"],
                "temperature": 0.1,
                "disable": false,
            }
        },
        "git_master": { "include_co_authored_by": false },
        "task": {},
        "teams": {
            "builders": {
                "description": "Build team",
                "members": [{ "name": "quick-one", "kind": "category", "category": "quick", "prompt": "Help" }]
            }
        },
    });
    let value = parsed(&omo_config_core::omo_config_schema(), config);
    assert_eq!(value["git_master"]["include_co_authored_by"], json!(false));
    assert_eq!(value["task"]["default_execution_mode"], json!("in-process"));
    assert_eq!(value["task"]["default_concurrency"], json!(5));
    assert_eq!(value["task"]["residency_max_children"], json!(8));
    assert_eq!(value["categories"]["deep"]["max_tokens"], json!(12000));
    assert_eq!(value["categories"]["deep"]["reasoning"], json!("high"));
    assert_eq!(
        value["categories"]["deep"]["provider_options"],
        json!({ "thinking": { "type": "enabled", "budgetTokens": 2048 }, "textVerbosity": "medium" })
    );
}

#[test]
fn config_schema_rejects_an_unknown_root_key() {
    assert!(
        safe_parse(
            &omo_config_core::omo_config_schema(),
            &json!({ "unknown_section": true })
        )
        .is_err()
    );
}

#[test]
fn config_schema_reports_the_bad_task_field_path() {
    let reported = issues(
        &omo_config_core::omo_config_schema(),
        json!({ "task": { "default_concurrency": "five" } }),
    );
    assert!(issue_paths(&reported).contains(&"task.default_concurrency".to_string()));
}

#[test]
fn unified_config_keeps_every_supported_section() {
    let config = json!({
        "models": { "sol": { "model": "openai/gpt-5.6-sol", "variant": "high", "reasoningEffort": "xhigh" } },
        "[opencode]": { "background_task": { "enabled": true } },
        "[senpi]": { "agents": { "oracle": { "model": "sol" } } },
        "[codex]": { "telemetry": { "enabled": false } },
        "profiles": {
            "focused": {
                "categories": { "deep": { "model": "sol" } },
                "models": { "sol": { "model": "openai/gpt-5.6-sol", "reasoningEffort": "high" } },
                "[opencode]": { "background_task": { "enabled": false } },
                "[senpi]": { "task": { "default_concurrency": 2 } },
                "[codex]": { "telemetry": { "enabled": true } },
            }
        },
        "_migrations": ["2026-07-opencode-config-unification"],
        "legacy_migrations": { "legacy-config": { "migrated": true } },
    });
    let value = parsed(&omo_config_core::omo_config_schema(), config);
    assert_eq!(
        value["models"]["sol"],
        json!({ "model": "openai/gpt-5.6-sol", "reasoning": "xhigh" })
    );
    assert_eq!(
        value["[opencode]"],
        json!({ "background_task": { "enabled": true } })
    );
    assert_eq!(value["[senpi]"]["agents"]["oracle"]["model"], json!("sol"));
    assert_eq!(value["[codex]"]["telemetry"]["enabled"], json!(false));
    assert_eq!(
        value["profiles"]["focused"]["[codex]"]["telemetry"]["enabled"],
        json!(true)
    );
    assert_eq!(
        value["_migrations"],
        json!(["2026-07-opencode-config-unification"])
    );
    assert_eq!(
        value["legacy_migrations"]["legacy-config"],
        json!({ "migrated": true })
    );
}

#[test]
fn unified_config_keeps_harness_and_profile_model_overlays_default_free() {
    let config = json!({
        "models": { "sol": { "model": "openai/gpt-5.6-sol" } },
        "[senpi]": { "models": { "sol": { "reasoningEffort": "high" } } },
        "profiles": {
            "focused": {
                "models": { "sol": { "variant": "low" } },
                "[codex]": { "models": { "sol": { "reasoningEffort": "minimal" } } },
            }
        },
    });
    let value = parsed(&omo_config_core::omo_config_schema(), config);
    assert_eq!(
        value["[senpi]"]["models"]["sol"],
        json!({ "reasoning": "high" })
    );
    assert_eq!(
        value["profiles"]["focused"]["models"]["sol"],
        json!({ "reasoning": "low" })
    );
    assert_eq!(
        value["profiles"]["focused"]["[codex]"]["models"]["sol"],
        json!({ "reasoning": "minimal" })
    );
}

#[test]
fn unified_config_rejects_unknown_root_and_profile_keys() {
    assert!(
        safe_parse(
            &omo_config_core::omo_config_schema(),
            &json!({ "unknown_key": 1 })
        )
        .is_err()
    );
    assert!(
        safe_parse(
            &omo_config_core::omo_config_schema(),
            &json!({ "profiles": { "focused": { "unknown_key": 1 } } })
        )
        .is_err()
    );
}

#[test]
fn unified_config_rejects_an_array_opencode_block_at_its_path() {
    let reported = issues(
        &omo_config_core::omo_config_schema(),
        json!({ "[opencode]": [] }),
    );
    assert!(
        issue_paths(&reported)
            .iter()
            .any(|path| path.contains("[opencode]"))
    );
}

#[test]
fn unified_config_keeps_the_empty_codex_block_default_free() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "telemetry": { "enabled": true }, "[codex]": { "telemetry": {} } }),
    );
    assert_eq!(value["telemetry"]["enabled"], json!(true));
    assert!(value["[codex]"]["telemetry"].get("enabled").is_none());
}

#[test]
fn task_settings_default_the_unavailable_category_warnings_on() {
    let value = parsed(&omo_config_core::omo_task_settings_schema(), json!({}));
    assert_eq!(value["warnings"]["unavailable_categories"], json!(true));
}

#[test]
fn task_settings_preserve_an_explicit_warning_suppression() {
    let value = parsed(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "warnings": { "unavailable_categories": false } }),
    );
    assert_eq!(value["warnings"]["unavailable_categories"], json!(false));
}

#[test]
fn task_settings_reject_a_non_boolean_warning_suppression_at_the_nested_path() {
    let reported = issues(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "warnings": { "unavailable_categories": "nope" } }),
    );
    assert!(
        issue_paths(&reported)
            .join(",")
            .contains("warnings.unavailable_categories")
    );
}

#[test]
fn task_settings_leave_reattach_absent_without_a_reconcile_override() {
    let value = parsed(&omo_config_core::omo_task_settings_schema(), json!({}));
    assert!(value.get("reattach_on_reconcile").is_none());
}

#[test]
fn task_settings_preserve_a_disabled_reattach_override() {
    let value = parsed(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "reattach_on_reconcile": false }),
    );
    assert_eq!(value["reattach_on_reconcile"], json!(false));
}

#[test]
fn task_settings_default_resume_children_on() {
    let value = parsed(&omo_config_core::omo_task_settings_schema(), json!({}));
    assert_eq!(value["resume_children"], json!(true));
}

#[test]
fn task_settings_preserve_an_explicit_resume_children_false() {
    let value = parsed(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "resume_children": false }),
    );
    assert_eq!(value["resume_children"], json!(false));
}

#[test]
fn task_settings_preserve_an_explicit_resume_children_true() {
    let value = parsed(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "resume_children": true }),
    );
    assert_eq!(value["resume_children"], json!(true));
}

#[test]
fn task_settings_reject_a_non_boolean_resume_children() {
    let reported = issues(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "resume_children": "yes" }),
    );
    assert!(issue_paths(&reported).join(",").contains("resume_children"));
}

#[test]
fn task_settings_fill_every_documented_dag_default() {
    let value = parsed(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "dag": {} }),
    );
    assert_eq!(
        value["dag"],
        json!({
            "max_nodes_per_run": 64,
            "max_runs_per_session": 16,
            "subscriber_ring": 1000,
            "heartbeat_ms": 15000,
            "history_default_limit": 256,
            "history_max_limit": 1000,
            "retention_days": 7,
            "max_prompt_bytes": 262144,
        })
    );
}

#[test]
fn task_settings_leave_dag_absent_when_the_block_is_omitted() {
    let value = parsed(&omo_config_core::omo_task_settings_schema(), json!({}));
    assert!(value.get("dag").is_none());
}

#[test]
fn task_settings_apply_a_partial_dag_override_beside_defaults() {
    let value = parsed(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "dag": { "max_nodes_per_run": 8, "heartbeat_ms": 500 } }),
    );
    assert_eq!(value["dag"]["max_nodes_per_run"], json!(8));
    assert_eq!(value["dag"]["heartbeat_ms"], json!(500));
    assert_eq!(value["dag"]["subscriber_ring"], json!(1000));
}

#[test]
fn task_settings_reject_an_unknown_dag_key_with_the_key_list() {
    let reported = issues(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "dag": { "max_nodes_per_run": 8, "wat": true } }),
    );
    let issue = reported
        .iter()
        .find(|issue| issue.path_string() == "dag")
        .expect("dag issue");
    assert_eq!(issue.code, omo_config_core::IssueCode::UnrecognizedKeys);
    assert_eq!(issue.keys, vec!["wat".to_string()]);
}

#[test]
fn task_settings_reject_a_non_positive_dag_bound_at_the_nested_path() {
    let reported = issues(
        &omo_config_core::omo_task_settings_schema(),
        json!({ "dag": { "max_nodes_per_run": 0 } }),
    );
    assert!(
        issue_paths(&reported)
            .join(",")
            .contains("dag.max_nodes_per_run")
    );
}

#[test]
fn task_settings_layer_keeps_a_partial_dag_without_defaults() {
    let value = parsed(
        &omo_config_core::omo_task_settings_layer_schema(),
        json!({ "dag": { "heartbeat_ms": 500 } }),
    );
    assert_eq!(value["dag"], json!({ "heartbeat_ms": 500 }));
}

#[test]
fn task_settings_layer_rejects_an_unknown_dag_key_with_the_key_list() {
    let reported = issues(
        &omo_config_core::omo_task_settings_layer_schema(),
        json!({ "dag": { "nope": 1 } }),
    );
    let issue = reported
        .iter()
        .find(|issue| issue.path_string() == "dag")
        .expect("dag issue");
    assert_eq!(issue.code, omo_config_core::IssueCode::UnrecognizedKeys);
    assert_eq!(issue.keys, vec!["nope".to_string()]);
}

fn full_memory_defaults() -> Value {
    json!({
        "enabled": true,
        "agent": "auto",
        "tool_exposure": "direct",
        "reflection": {
            "enabled": true,
            "trigger": { "step_count": 25, "on_compaction": true },
            "merge": "auto",
            "category": "quick",
            "timeout_minutes": 15,
            "sandbox": "auto",
        },
        "nudge": { "enabled": true, "every_user_turns": 10 },
        "facts": { "enabled": true, "debounce_settles": 4 },
        "dream": {
            "enabled": true,
            "idle_minutes": 30,
            "min_hours_between": 24,
            "shutdown_launch": true,
            "auto_select_max": 5,
            "auto_select_max_chars": 150000,
        },
        "people": { "enabled": true, "max_entries": 40, "max_entry_chars": 200 },
        "soul": { "edit_notice": true },
        "write_notice": { "enabled": true },
        "sync": { "enabled": true },
        "search": { "enabled": true },
        "recall": {
            "enabled": true,
            "max_items": 2,
            "category": "quick",
            "event_caps": { "tool_args": 400, "result_head": 600, "assistant": 1500, "prompt": 4000 },
            "sidecar_max_tokens": 48000,
            "max_concurrent_wakes": 2,
            "tool_budget": 8,
            "query_expansion": false,
        },
        "compile_warn_tokens": 30000,
        "agents": {},
    })
}

#[test]
fn memory_settings_apply_the_pinned_v2_defaults() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(value, full_memory_defaults());
}

#[test]
fn memory_settings_preserve_a_fully_specified_block() {
    let input = json!({
        "enabled": false,
        "agent": "backend-lead",
        "tool_exposure": "search",
        "reflection": {
            "enabled": false,
            "trigger": { "step_count": 25, "on_compaction": false },
            "merge": "integration",
            "category": "deep",
            "timeout_minutes": 30,
            "sandbox": "required",
        },
        "nudge": { "enabled": false, "every_user_turns": 5 },
        "facts": { "enabled": false, "debounce_settles": 2 },
        "dream": {
            "enabled": false,
            "idle_minutes": 0,
            "min_hours_between": 12,
            "shutdown_launch": false,
            "auto_select_max": 3,
            "auto_select_max_chars": 100000,
        },
        "people": { "enabled": false, "max_entries": 20, "max_entry_chars": 100 },
        "soul": { "edit_notice": false },
        "write_notice": { "enabled": false },
        "sync": { "remote": "file:///tmp/memory-mirror.git", "enabled": true },
        "search": { "enabled": false },
        "recall": {
            "enabled": false,
            "max_items": 4,
            "category": "deep",
            "event_caps": { "tool_args": 400, "result_head": 600, "assistant": 1500, "prompt": 4000 },
            "sidecar_max_tokens": 48000,
            "max_concurrent_wakes": 2,
            "tool_budget": 8,
            "query_expansion": false,
        },
        "compile_warn_tokens": 50000,
        "agents": {
            "backend-lead": { "enabled": true, "reflection": { "trigger": { "step_count": 10 }, "category": "quick" } }
        },
    });
    assert_eq!(
        parsed(
            &omo_config_core::omo_memory_settings_schema(),
            input.clone()
        ),
        input
    );
}

#[test]
fn memory_settings_default_the_reflection_trigger() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(value["reflection"]["trigger"]["step_count"], json!(25));
    assert_eq!(value["reflection"]["enabled"], json!(true));
}

#[test]
fn memory_settings_preserve_an_explicit_zero_step_count() {
    let value = parsed(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "reflection": { "trigger": { "step_count": 0 } } }),
    );
    assert_eq!(value["reflection"]["trigger"]["step_count"], json!(0));
}

#[test]
fn memory_settings_reject_a_negative_step_count_at_the_trigger_path() {
    let reported = issues(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "reflection": { "trigger": { "step_count": -1 } } }),
    );
    assert!(
        issue_paths(&reported)
            .join(",")
            .contains("reflection.trigger.step_count")
    );
}

#[test]
fn memory_settings_reject_a_non_boolean_compaction_trigger_at_the_trigger_path() {
    let reported = issues(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "reflection": { "trigger": { "on_compaction": "yes" } } }),
    );
    assert!(
        issue_paths(&reported)
            .join(",")
            .contains("reflection.trigger.on_compaction")
    );
}

#[test]
fn memory_settings_reject_unknown_keys_at_both_levels() {
    let node = omo_config_core::omo_memory_settings_schema();
    assert!(safe_parse(&node, &json!({ "enabled": true, "bogus": true })).is_err());
    assert!(safe_parse(&node, &json!({ "reflection": { "bogus": true } })).is_err());
}

#[test]
fn memory_settings_keep_per_agent_overrides_default_free() {
    let value = parsed(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "agents": { "backend-lead": { "enabled": false, "reflection": { "trigger": { "step_count": 10 } } } } }),
    );
    assert_eq!(
        value["agents"]["backend-lead"],
        json!({ "enabled": false, "reflection": { "trigger": { "step_count": 10 } } })
    );
}

#[test]
fn memory_settings_reject_an_unknown_key_inside_an_agent_override() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "agents": { "backend-lead": { "bogus": true } } })
        )
        .is_err()
    );
}

#[test]
fn memory_settings_carry_a_per_agent_dream_override() {
    let value = parsed(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "agents": { "research-agent": { "dream": { "idle_minutes": 60 } } } }),
    );
    assert_eq!(
        value["agents"]["research-agent"]["dream"],
        json!({ "idle_minutes": 60 })
    );
}

#[test]
fn memory_settings_carry_a_per_agent_nudge_override() {
    let value = parsed(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "agents": { "research-agent": { "nudge": { "every_user_turns": 20 } } } }),
    );
    assert_eq!(
        value["agents"]["research-agent"]["nudge"],
        json!({ "every_user_turns": 20 })
    );
}

#[test]
fn memory_settings_layer_stays_a_default_free_deep_partial() {
    let value = parsed(
        &omo_config_core::omo_memory_settings_layer_schema(),
        json!({ "reflection": { "category": "deep" } }),
    );
    assert_eq!(value, json!({ "reflection": { "category": "deep" } }));
}

#[test]
fn memory_settings_layer_rejects_an_unknown_key() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_layer_schema(),
            &json!({ "bogus": 1 })
        )
        .is_err()
    );
}

#[test]
fn memory_settings_layer_accepts_the_v2_block_keys_as_deep_partials() {
    let input = json!({
        "nudge": { "every_user_turns": 5 },
        "facts": { "debounce_settles": 2 },
        "dream": { "idle_minutes": 0 },
        "people": { "max_entries": 20 },
        "soul": { "edit_notice": false },
    });
    assert_eq!(
        parsed(
            &omo_config_core::omo_memory_settings_layer_schema(),
            input.clone()
        ),
        input
    );
}

#[test]
fn memory_config_wiring_materializes_defaults_once() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "memory": {} }),
    );
    let memory = &value["memory"];
    assert_eq!(memory["enabled"], json!(true));
    assert_eq!(memory["reflection"]["enabled"], json!(true));
    assert_eq!(memory["reflection"]["category"], json!("quick"));
    assert_eq!(
        memory["reflection"]["trigger"],
        json!({ "step_count": 25, "on_compaction": true })
    );
    assert_eq!(
        memory["nudge"],
        json!({ "enabled": true, "every_user_turns": 10 })
    );
    assert_eq!(
        memory["facts"],
        json!({ "enabled": true, "debounce_settles": 4 })
    );
    assert_eq!(
        memory["dream"],
        json!({
            "enabled": true,
            "idle_minutes": 30,
            "min_hours_between": 24,
            "shutdown_launch": true,
            "auto_select_max": 5,
            "auto_select_max_chars": 150000,
        })
    );
    assert_eq!(
        memory["people"],
        json!({ "enabled": true, "max_entries": 40, "max_entry_chars": 200 })
    );
    assert_eq!(memory["soul"], json!({ "edit_notice": true }));
}

#[test]
fn memory_config_wiring_keeps_harness_and_profile_blocks_default_free() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({
            "memory": { "reflection": { "timeout_minutes": 20 } },
            "[senpi]": { "memory": { "reflection": { "category": "deep" } } },
            "profiles": { "focused": { "memory": { "search": { "enabled": false } } } },
        }),
    );
    assert_eq!(
        value["[senpi]"]["memory"],
        json!({ "reflection": { "category": "deep" } })
    );
    assert_eq!(
        value["profiles"]["focused"]["memory"],
        json!({ "search": { "enabled": false } })
    );
}

#[test]
fn memory_config_wiring_rejects_an_unknown_key_inside_a_profile_memory_block() {
    assert!(
        safe_parse(
            &omo_config_core::omo_config_schema(),
            &json!({ "profiles": { "focused": { "memory": { "bogus": 1 } } } })
        )
        .is_err()
    );
}

#[test]
fn memory_profile_and_harness_blocks_deep_merge() {
    let config = json!({
        "memory": { "enabled": true, "reflection": { "timeout_minutes": 20, "category": "deep" } },
        "[senpi]": { "memory": { "reflection": { "category": "quick" } } },
    })
    .as_object()
    .cloned()
    .expect("object");
    let result =
        omo_config_core::resolve_omo_config_view(omo_config_core::ResolveOmoConfigViewOptions {
            config: &config,
            harness: Some(&"senpi".to_string()),
            profile: None,
        });
    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(
        result.config["memory"],
        json!({ "enabled": true, "reflection": { "timeout_minutes": 20, "category": "quick" } })
    );
}

#[test]
fn memory_profile_layers_win_per_leaf() {
    let config = json!({
        "memory": { "agent": "auto", "reflection": { "timeout_minutes": 15 } },
        "[senpi]": { "memory": { "reflection": { "category": "deep" } } },
        "profiles": {
            "focused": {
                "memory": { "reflection": { "timeout_minutes": 30 } },
                "[senpi]": { "memory": { "search": { "enabled": false } } },
            }
        },
    })
    .as_object()
    .cloned()
    .expect("object");
    let result =
        omo_config_core::resolve_omo_config_view(omo_config_core::ResolveOmoConfigViewOptions {
            config: &config,
            harness: Some(&"senpi".to_string()),
            profile: Some(&"focused".to_string()),
        });
    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(
        result.config["memory"],
        json!({
            "agent": "auto",
            "reflection": { "timeout_minutes": 30, "category": "deep" },
            "search": { "enabled": false },
        })
    );
}

#[test]
fn memory_v2_blocks_default_nudge() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(
        value["nudge"],
        json!({ "enabled": true, "every_user_turns": 10 })
    );
}

#[test]
fn memory_v2_blocks_reject_a_zero_turn_interval() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "nudge": { "every_user_turns": 0 } })
        )
        .is_err()
    );
}

#[test]
fn memory_v2_blocks_default_facts() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(
        value["facts"],
        json!({ "enabled": true, "debounce_settles": 4 })
    );
}

#[test]
fn memory_v2_blocks_reject_a_category_inside_facts() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "facts": { "category": "quick" } })
        )
        .is_err()
    );
}

#[test]
fn memory_v2_blocks_reject_a_zero_debounce() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "facts": { "debounce_settles": 0 } })
        )
        .is_err()
    );
}

#[test]
fn memory_v2_blocks_default_dream() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(
        value["dream"],
        json!({
            "enabled": true,
            "idle_minutes": 30,
            "min_hours_between": 24,
            "shutdown_launch": true,
            "auto_select_max": 5,
            "auto_select_max_chars": 150000,
        })
    );
}

#[test]
fn memory_v2_blocks_allow_a_zero_idle_minutes() {
    let value = parsed(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "dream": { "idle_minutes": 0 } }),
    );
    assert_eq!(value["dream"]["idle_minutes"], json!(0));
}

#[test]
fn memory_v2_blocks_reject_a_below_one_min_hours_between() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "dream": { "min_hours_between": 0 } })
        )
        .is_err()
    );
}

#[test]
fn memory_v2_blocks_bound_auto_select_max_to_one_through_ten() {
    let node = omo_config_core::omo_memory_settings_schema();
    assert!(safe_parse(&node, &json!({ "dream": { "auto_select_max": 0 } })).is_err());
    assert!(safe_parse(&node, &json!({ "dream": { "auto_select_max": 11 } })).is_err());
}

#[test]
fn memory_v2_blocks_reject_a_below_floor_auto_select_max_chars() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "dream": { "auto_select_max_chars": 9999 } })
        )
        .is_err()
    );
}

#[test]
fn memory_v2_blocks_default_people() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(
        value["people"],
        json!({ "enabled": true, "max_entries": 40, "max_entry_chars": 200 })
    );
}

#[test]
fn memory_v2_blocks_bound_people_max_entries() {
    let node = omo_config_core::omo_memory_settings_schema();
    assert!(safe_parse(&node, &json!({ "people": { "max_entries": 0 } })).is_err());
    assert!(safe_parse(&node, &json!({ "people": { "max_entries": 101 } })).is_err());
}

#[test]
fn memory_v2_blocks_bound_people_max_entry_chars() {
    let node = omo_config_core::omo_memory_settings_schema();
    assert!(safe_parse(&node, &json!({ "people": { "max_entry_chars": 49 } })).is_err());
    assert!(safe_parse(&node, &json!({ "people": { "max_entry_chars": 501 } })).is_err());
}

#[test]
fn memory_v2_blocks_default_soul() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(value["soul"], json!({ "edit_notice": true }));
}

#[test]
fn memory_v2_blocks_reject_an_unknown_soul_key() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "soul": { "bogus": true } })
        )
        .is_err()
    );
}

#[test]
fn memory_v2_blocks_reject_unknown_keys_in_every_block() {
    let node = omo_config_core::omo_memory_settings_schema();
    for input in [
        json!({ "nudge": { "bogus": true } }),
        json!({ "facts": { "bogus": true } }),
        json!({ "dream": { "bogus": true } }),
        json!({ "people": { "bogus": true } }),
    ] {
        assert!(
            safe_parse(&node, &input).is_err(),
            "expected {input} to fail"
        );
    }
}

#[test]
fn team_spec_requires_a_lead_when_the_team_has_multiple_members() {
    let node = omo_config_core::omo_team_spec_schema();
    let reported = issues(
        &node,
        json!({
            "members": [
                { "name": "one", "kind": "category", "category": "quick", "prompt": "go" },
                { "name": "two", "kind": "subagent_type", "subagent_type": "explore" },
            ]
        }),
    );
    let issue = reported
        .iter()
        .find(|issue| issue.path_string() == "leadAgentId")
        .expect("leadAgentId issue");
    assert_eq!(issue.code, omo_config_core::IssueCode::Custom);
    assert_eq!(
        issue.message,
        "leadAgentId required when a team has multiple members"
    );
}

#[test]
fn team_spec_layer_drops_member_requirements_and_the_refinement() {
    let node = omo_config_core::omo_team_spec_layer_schema();
    let value = parsed(&node, json!({ "description": "partial" }));
    assert_eq!(value, json!({ "description": "partial" }));
}

#[test]
fn team_spec_rejects_a_member_name_outside_the_slug_alphabet() {
    let node = omo_config_core::omo_team_spec_schema();
    assert!(safe_parse(
        &node,
        &json!({ "members": [{ "name": "One Two", "kind": "category", "category": "quick", "prompt": "go" }] })
    )
    .is_err());
}

fn load_user_config(
    home: &std::path::Path,
    config: &str,
    harness: Option<&str>,
) -> omo_config_core::LoadOmoConfigResult {
    std::fs::create_dir_all(home.join(".maho")).expect("config dir");
    std::fs::create_dir_all(home.join("project")).expect("project dir");
    std::fs::write(home.join(".maho/omo.jsonc"), config).expect("write config");
    let home_dir = home.to_string_lossy().into_owned();
    omo_config_core::load_omo_config(&omo_config_core::LoadOmoConfigOptions {
        cwd: Some(home.join("project").to_string_lossy().into_owned()),
        env: Some(std::collections::BTreeMap::from([(
            "HOME".to_string(),
            home_dir,
        )])),
        harness: harness.map(str::to_string),
        platform: Some("linux".to_string()),
        ..Default::default()
    })
}

#[test]
fn computer_settings_round_trip_every_key() {
    let block = json!({
        "enabled": true,
        "display": "all",
        "max_width": 1920,
        "max_height": 1080,
        "screenshot_max_bytes": 1_000_000,
        "stop_hotkey": "ctrl+alt+shift+escape",
        "allow_host_relay_only_stop": false,
        "macos_canary": "off",
        "audit_log": { "enabled": false },
        "screenshot_gc": { "enabled": true, "stale_ms": 0, "scan_interval_ms": 60_000 },
        "engine_path": "/opt/engine",
        "cua_adapter": true,
    });
    assert_eq!(
        parsed(
            &omo_config_core::omo_computer_settings_schema(),
            block.clone()
        ),
        block
    );
}

#[test]
fn computer_settings_reject_an_unknown_or_camel_case_key() {
    assert!(
        safe_parse(
            &omo_config_core::omo_computer_settings_schema(),
            &json!({ "cuaAdapter": true })
        )
        .is_err()
    );
}

#[test]
fn computer_root_config_keeps_the_block() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "computer": { "enabled": false } }),
    );
    assert_eq!(value["computer"], json!({ "enabled": false }));
}

#[test]
fn computer_harness_support_is_native_only() {
    for setting_path in [
        "computer.enabled",
        "computer.display",
        "computer.max_width",
        "computer.max_height",
        "computer.screenshot_max_bytes",
        "computer.stop_hotkey",
        "computer.allow_host_relay_only_stop",
        "computer.macos_canary",
        "computer.audit_log",
        "computer.screenshot_gc",
        "computer.engine_path",
        "computer.cua_adapter",
    ] {
        let supported = omo_config_core::computer_setting_harness_support(setting_path);
        assert_eq!(
            supported.map(<[&str]>::to_vec),
            Some(vec!["native"]),
            "{setting_path}"
        );
    }
}

#[test]
fn gateway_key_is_accepted_by_the_root_layer_and_native_harness_schemas() {
    let section = json!({
        "scopes": [{ "id": "qa", "surfaces": [{ "platform": "slack", "account_id": "T000TEST", "options": { "token_kind": "app" } }] }],
        "stt": { "provider": "p" },
    });
    let document = json!({ "gateway": section, "[native]": { "gateway": section } });
    let root = parsed(&omo_config_core::omo_config_schema(), document.clone());
    let layer = parsed(&omo_config_core::omo_config_layer_schema(), document.clone());
    assert_eq!(root["gateway"], section);
    assert_eq!(layer["gateway"], section);
    assert_eq!(root["[native]"]["gateway"], section);
}

#[test]
fn gateway_non_object_is_rejected_at_its_path() {
    let reported = issues(
        &omo_config_core::omo_config_layer_schema(),
        json!({ "gateway": ["not", "an", "object"] }),
    );
    assert_eq!(issue_paths(&reported), vec!["gateway".to_string()]);
}

#[test]
fn gateway_user_config_loads_without_diagnostics() {
    let home = tempfile::tempdir().expect("tempdir");
    let section = json!({ "scopes": [{ "id": "qa" }], "stt": { "provider": "p" } });
    let result = load_user_config(
        home.path(),
        &format!(
            "// user config\n{}",
            json!({ "gateway": section, "disabled_skills": ["kept"] })
        ),
        None,
    );
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(result.config["gateway"], section);
    assert_eq!(result.config["disabled_skills"], json!(["kept"]));
}

#[test]
fn config_schema_defaults_an_empty_format_on_mutation_block() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "formatOnMutation": {} }),
    );
    assert_eq!(
        value["formatOnMutation"],
        json!({ "mode": "best-effort", "maxFileBytes": 1_048_576, "timeoutMs": 3_000 })
    );
}

#[test]
fn config_schema_keeps_format_on_mutation_overrides_and_languages() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "formatOnMutation": { "mode": "required", "languages": { "python": false }, "timeoutMs": 1000 } }),
    );
    assert_eq!(
        value["formatOnMutation"],
        json!({ "mode": "required", "languages": { "python": false }, "maxFileBytes": 1_048_576, "timeoutMs": 1000 })
    );
}

#[test]
fn config_schema_rejects_a_format_on_mutation_mode_outside_the_enum() {
    assert!(
        safe_parse(
            &omo_config_core::omo_config_schema(),
            &json!({ "formatOnMutation": { "mode": "sometimes" } })
        )
        .is_err()
    );
}

#[test]
fn git_master_empty_section_defaults_attribution_off() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "git_master": {} }),
    );
    assert_eq!(
        value["git_master"],
        json!({ "commit_footer": false, "include_co_authored_by": false })
    );
}

#[test]
fn git_master_preserves_a_custom_footer_and_disabled_co_author() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "git_master": { "commit_footer": "Shipped with omo", "include_co_authored_by": false } }),
    );
    assert_eq!(
        value["git_master"]["commit_footer"],
        json!("Shipped with omo")
    );
    assert_eq!(value["git_master"]["include_co_authored_by"], json!(false));
}

#[test]
fn git_master_accepts_harness_and_profile_layer_overrides() {
    assert!(
        safe_parse(
            &omo_config_core::omo_config_schema(),
            &json!({ "[senpi]": { "git_master": { "include_co_authored_by": false } } })
        )
        .is_ok()
    );
    assert!(
        safe_parse(
            &omo_config_core::omo_config_schema(),
            &json!({ "profiles": { "work": { "git_master": { "commit_footer": false } } } })
        )
        .is_ok()
    );
}

#[test]
fn git_master_layer_injects_no_defaults() {
    let value = parsed(
        &omo_config_core::omo_config_layer_schema(),
        json!({ "git_master": {} }),
    );
    assert_eq!(value["git_master"], json!({}));
}

#[test]
fn git_master_rejects_an_unknown_key() {
    assert!(
        safe_parse(
            &omo_config_core::omo_config_schema(),
            &json!({ "git_master": { "co_author": "someone" } })
        )
        .is_err()
    );
}

#[test]
fn git_master_resolver_always_returns_both_keys() {
    let resolved = omo_config_core::resolve_omo_git_master_settings(&json!({}));
    assert_eq!(
        resolved,
        json!({ "commit_footer": false, "include_co_authored_by": false })
    );
    let explicit = omo_config_core::resolve_omo_git_master_settings(
        &json!({ "git_master": { "commit_footer": true } }),
    );
    assert_eq!(
        explicit,
        json!({ "commit_footer": true, "include_co_authored_by": false })
    );
    let custom = omo_config_core::resolve_omo_git_master_settings(
        &json!({ "git_master": { "commit_footer": "Shipped with omo", "include_co_authored_by": true } }),
    );
    assert_eq!(
        custom,
        json!({ "commit_footer": "Shipped with omo", "include_co_authored_by": true })
    );
}

#[test]
fn git_master_harness_support_is_native_only() {
    for setting_path in ["git_master.commit_footer", "git_master.include_co_authored_by"] {
        let supported = omo_config_core::git_master_setting_harness_support(setting_path);
        assert_eq!(
            supported.map(<[&str]>::to_vec),
            Some(vec!["native"]),
            "{setting_path}"
        );
    }
}

#[test]
fn model_profile_accepts_a_display_name_only_entry() {
    let value = parsed(
        &omo_config_core::omo_model_profile_schema(),
        json!({ "display_name": "Capable" }),
    );
    assert_eq!(value, json!({ "display_name": "Capable" }));
}

#[test]
fn model_profile_keeps_catalog_names_and_tuned_entries() {
    let entry = json!({ "display_name": "Deep work", "models": ["astra", { "model": "openai/gpt-5.6-sol", "reasoning": "medium" }] });
    assert_eq!(
        parsed(&omo_config_core::omo_model_profile_schema(), entry.clone()),
        entry
    );
}

#[test]
fn model_profile_normalizes_a_legacy_variant_on_a_chain_entry() {
    let value = parsed(
        &omo_config_core::omo_model_profile_schema(),
        json!({ "models": [{ "model": "openai/gpt-6-astra", "variant": "high" }] }),
    );
    assert_eq!(
        value["models"],
        json!([{ "model": "openai/gpt-6-astra", "reasoning": "high" }])
    );
}

#[test]
fn model_profile_rejects_an_unknown_sibling_key() {
    assert!(
        safe_parse(
            &omo_config_core::omo_model_profile_schema(),
            &json!({ "display_name": "Capable", "model": "anthropic/" })
        )
        .is_err()
    );
}

#[test]
fn model_profiles_record_parses_every_named_profile() {
    let value = parsed(
        &omo_config_core::omo_model_profiles_schema(),
        json!({ "capable": { "display_name": "Capable", "models": ["anthropic/"] }, "simple-work": { "models": ["openai/gpt-5.6-luna-fast"] } }),
    );
    let keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, vec!["capable", "simple-work"]);
}

#[test]
fn model_profile_layer_accepts_partial_entries_and_rejects_unknown_keys() {
    assert!(
        safe_parse(
            &omo_config_core::omo_model_profile_layer_schema(),
            &json!({ "display_name": "Capable" })
        )
        .is_ok()
    );
    assert!(
        safe_parse(
            &omo_config_core::omo_model_profiles_layer_schema(),
            &json!({ "capable": { "models": [], "reasoning": "high" } })
        )
        .is_err()
    );
}

#[test]
fn config_schema_accepts_model_profiles_and_a_selected_profile() {
    let block = json!({
        "model_profiles": { "capable": { "display_name": "Capable", "family": "daily", "tier": "normal", "models": ["anthropic/", { "model": "openai/gpt-6-astra", "reasoning": "high" }] } },
        "model_profile": "capable",
    });
    let value = parsed(&omo_config_core::omo_config_schema(), block.clone());
    assert_eq!(value["model_profile"], json!("capable"));
    assert_eq!(
        value["model_profiles"]["capable"]["display_name"],
        json!("Capable")
    );
    assert!(safe_parse(&omo_config_core::omo_typed_harness_config_schema(), &block).is_ok());
    assert!(safe_parse(&omo_config_core::omo_config_layer_schema(), &block).is_ok());
    assert!(safe_parse(&omo_config_core::omo_config_profile_schema(), &block).is_ok());
}

#[test]
fn config_schema_rejects_a_non_string_model_profile_at_its_path() {
    let reported = issues(
        &omo_config_core::omo_config_schema(),
        json!({ "model_profile": 123 }),
    );
    assert!(issue_paths(&reported).contains(&"model_profile".to_string()));
}

#[test]
fn config_schema_reports_a_bad_git_master_field_path() {
    let reported = issues(
        &omo_config_core::omo_config_schema(),
        json!({ "git_master": { "include_co_authored_by": "yes" } }),
    );
    assert!(issue_paths(&reported).contains(&"git_master.include_co_authored_by".to_string()));
}

#[test]
fn model_profile_display_name_only_override_survives_a_user_config_load() {
    let home = tempfile::tempdir().expect("tempdir");
    let result = load_user_config(
        home.path(),
        r#"{"categories":{"quick":{"model":"user-model"}},"model_profiles":{"capable":{"display_name":"Fast and capable"}},"model_profile":"capable"}"#,
        Some("senpi"),
    );
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(
        result.config["categories"]["quick"]["model"],
        json!("user-model")
    );
    assert_eq!(
        result.config["model_profiles"]["capable"],
        json!({ "display_name": "Fast and capable" })
    );
    assert_eq!(result.config["model_profile"], json!("capable"));
}

#[test]
fn model_profile_inside_a_native_block_selects_the_profile() {
    let home = tempfile::tempdir().expect("tempdir");
    let result = load_user_config(
        home.path(),
        r#"{"model_profile":"simple-work","[native]":{"model_profile":"deep-work","model_profiles":{"deep-work":{"models":["openai/gpt-6-astra"]}}}}"#,
        Some("senpi"),
    );
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(result.config["model_profile"], json!("deep-work"));
    assert_eq!(
        result.config["model_profiles"]["deep-work"]["models"],
        json!(["openai/gpt-6-astra"])
    );
}

#[test]
fn canonical_category_name_renames_deep_to_deep_low() {
    assert_eq!(omo_config_core::canonical_category_name("deep"), "deep-low");
}

#[test]
fn canonical_category_name_keeps_live_names() {
    for name in [
        "deep-low",
        "deep-high",
        "quick",
        "ultrabrain",
        "my-custom-lane",
    ] {
        assert_eq!(omo_config_core::canonical_category_name(name), name);
    }
}

#[test]
fn canonicalize_legacy_category_names_renames_every_layer_and_reports() {
    let document = json!({
        "categories": { "deep": { "model": "a/b" }, "quick": { "model": "c/d" } },
        "[senpi]": { "categories": { "deep": { "reasoning": "high" } } },
        "[opencode]": { "categories": { "deep": { "model": "e/f" } } },
        "[codex]": { "categories": { "deep": { "model": "g/h" } } },
        "profiles": {
            "kimi": {
                "categories": { "deep": { "model": "i/j" } },
                "[senpi]": { "categories": { "deep": { "model": "k/l" } } },
            },
        },
    });
    let result = omo_config_core::canonicalize_legacy_category_names(&document);
    assert_eq!(
        Value::Object(result.document.clone()),
        json!({
            "categories": { "deep-low": { "model": "a/b" }, "quick": { "model": "c/d" } },
            "[senpi]": { "categories": { "deep-low": { "reasoning": "high" } } },
            "[opencode]": { "categories": { "deep-low": { "model": "e/f" } } },
            "[codex]": { "categories": { "deep-low": { "model": "g/h" } } },
            "profiles": {
                "kimi": {
                    "categories": { "deep-low": { "model": "i/j" } },
                    "[senpi]": { "categories": { "deep-low": { "model": "k/l" } } },
                },
            },
        })
    );
    let paths: Vec<&str> = result
        .renames
        .iter()
        .map(|rename| rename.path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec![
            "categories.deep",
            "[senpi].categories.deep",
            "[opencode].categories.deep",
            "[codex].categories.deep",
            "profiles.kimi.categories.deep",
            "profiles.kimi.[senpi].categories.deep",
        ]
    );
}

#[test]
fn canonicalize_legacy_category_names_renames_a_category_value() {
    let document = json!({
        "teams": {
            "reviewers": {
                "leadAgentId": "lead",
                "members": [
                    { "name": "one", "kind": "category", "category": "deep", "prompt": "go" },
                    { "name": "two", "kind": "category", "category": "quick", "prompt": "go" },
                ],
            },
        },
        "[senpi]": { "memory": { "reflection": { "category": "deep" } } },
    });
    let result = omo_config_core::canonicalize_legacy_category_names(&document);
    assert_eq!(
        Value::Object(result.document.clone()),
        json!({
            "teams": {
                "reviewers": {
                    "leadAgentId": "lead",
                    "members": [
                        { "name": "one", "kind": "category", "category": "deep-low", "prompt": "go" },
                        { "name": "two", "kind": "category", "category": "quick", "prompt": "go" },
                    ],
                },
            },
            "[senpi]": { "memory": { "reflection": { "category": "deep-low" } } },
        })
    );
    let paths: Vec<&str> = result
        .renames
        .iter()
        .map(|rename| rename.path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec![
            "teams.reviewers.members.0.category",
            "[senpi].memory.reflection.category",
        ]
    );
}

#[test]
fn canonicalize_legacy_category_names_keeps_the_canonical_entry() {
    let document = json!({ "categories": { "deep": { "model": "legacy/model" }, "deep-low": { "model": "canonical/model" } } });
    let result = omo_config_core::canonicalize_legacy_category_names(&document);
    assert_eq!(
        Value::Object(result.document),
        json!({ "categories": { "deep-low": { "model": "canonical/model" } } })
    );
    assert_eq!(result.renames.len(), 1);
    assert!(result.renames[0].dropped);
    assert_eq!(result.renames[0].path, "categories.deep");
}

#[test]
fn canonicalize_legacy_category_names_leaves_a_clean_document_untouched() {
    let document = json!({
        "categories": { "deep-low": { "model": "a/b" }, "deep-high": { "model": "c/d" } },
        "teams": { "r": { "members": [{ "name": "one", "kind": "category", "category": "deep-high", "prompt": "go" }] } },
    });
    let result = omo_config_core::canonicalize_legacy_category_names(&document);
    assert_eq!(Value::Object(result.document), document);
    assert!(result.renames.is_empty());
    assert!(!omo_config_core::has_legacy_category_names(&document));
}

#[test]
fn canonicalize_legacy_category_names_never_rewrites_free_text() {
    let document = json!({ "categories": { "deep-low": { "prompt_append": "route deep work here; deep means deep" } } });
    let result = omo_config_core::canonicalize_legacy_category_names(&document);
    assert_eq!(Value::Object(result.document), document);
    assert!(result.renames.is_empty());
}

#[test]
fn canonical_harness_name_renames_senpi_to_native() {
    assert_eq!(omo_config_core::canonical_harness_name("senpi"), "native");
}

#[test]
fn canonical_harness_name_keeps_live_ids() {
    for id in ["native", "opencode", "codex", "omo"] {
        assert_eq!(omo_config_core::canonical_harness_name(id), id);
    }
}

#[test]
fn canonicalize_legacy_harness_blocks_renames_root_and_profile() {
    let document = json!({
        "categories": { "quick": { "model": "a/b" } },
        "[senpi]": { "categories": { "quick": { "reasoning": "high" } } },
        "[opencode]": { "categories": { "quick": { "model": "e/f" } } },
        "profiles": {
            "kimi": {
                "[senpi]": { "model_profile": "kimi" },
            },
        },
    });
    let result = omo_config_core::canonicalize_legacy_harness_blocks(&document);
    assert_eq!(
        Value::Object(result.document.clone()),
        json!({
            "categories": { "quick": { "model": "a/b" } },
            "[native]": { "categories": { "quick": { "reasoning": "high" } } },
            "[opencode]": { "categories": { "quick": { "model": "e/f" } } },
            "profiles": {
                "kimi": {
                    "[native]": { "model_profile": "kimi" },
                },
            },
        })
    );
    let paths: Vec<&str> = result
        .renames
        .iter()
        .map(|rename| rename.path.as_str())
        .collect();
    assert_eq!(paths, vec!["[senpi]", "profiles.kimi.[senpi]"]);
}

#[test]
fn canonicalize_legacy_harness_blocks_keeps_the_canonical_block() {
    let document = json!({
        "[native]": { "model_profile": "canonical" },
        "[senpi]": { "model_profile": "legacy" },
    });
    let result = omo_config_core::canonicalize_legacy_harness_blocks(&document);
    assert_eq!(
        Value::Object(result.document),
        json!({ "[native]": { "model_profile": "canonical" } })
    );
    assert_eq!(result.renames.len(), 1);
    assert!(result.renames[0].dropped);
    assert_eq!(result.renames[0].path, "[senpi]");
}

#[test]
fn canonicalize_legacy_harness_blocks_leaves_a_clean_document_untouched() {
    let document = json!({
        "categories": { "quick": { "model": "a/b" } },
        "[native]": { "model_profile": "kimi" },
        "profiles": { "kimi": { "[codex]": { "model_profile": "kimi" } } },
    });
    let result = omo_config_core::canonicalize_legacy_harness_blocks(&document);
    assert_eq!(Value::Object(result.document), document);
    assert!(result.renames.is_empty());
    assert!(!omo_config_core::has_legacy_harness_blocks(&document));
}

#[test]
fn has_legacy_harness_blocks_reports_the_legacy_spelling() {
    assert!(omo_config_core::has_legacy_harness_blocks(
        &json!({ "[senpi]": { "model_profile": "kimi" } })
    ));
    assert!(omo_config_core::has_legacy_harness_blocks(
        &json!({ "profiles": { "kimi": { "[senpi]": { "model_profile": "kimi" } } } })
    ));
}

#[test]
fn native_harness_block_applies_to_the_native_view() {
    let home = tempfile::tempdir().expect("tempdir");
    let result = load_user_config(
        home.path(),
        r#"{
            "categories": { "quick": { "model": "base/model" } },
            "[native]": {
                "categories": { "quick": { "model": "native/model" } },
                "model_profile": "native-profile"
            }
        }"#,
        Some("native"),
    );
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(result.config["categories"]["quick"]["model"], json!("native/model"));
    assert_eq!(result.config["model_profile"], json!("native-profile"));
}

#[test]
fn legacy_senpi_harness_block_survives_and_is_reported() {
    let home = tempfile::tempdir().expect("tempdir");
    let result = load_user_config(
        home.path(),
        r#"{
            "categories": { "quick": { "model": "base/model" } },
            "[senpi]": {
                "categories": { "quick": { "model": "legacy/model", "reasoningEffort": "high" } },
                "git_master": { "commit_footer": true },
                "telemetry": { "enabled": false },
                "model_profile": "legacy-profile"
            }
        }"#,
        Some("native"),
    );
    assert_eq!(result.config["categories"]["quick"]["model"], json!("legacy/model"));
    assert_eq!(result.config["categories"]["quick"]["reasoning"], json!("high"));
    assert_eq!(result.config["git_master"]["commit_footer"], json!(true));
    assert_eq!(result.config["telemetry"]["enabled"], json!(false));
    assert_eq!(result.config["model_profile"], json!("legacy-profile"));
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, "deprecated-keys");
    assert_eq!(
        result.diagnostics[0].issue_paths,
        vec!["[senpi]".to_string()]
    );
}

#[test]
fn native_block_wins_over_the_legacy_senpi_block() {
    let home = tempfile::tempdir().expect("tempdir");
    let result = load_user_config(
        home.path(),
        r#"{
            "[native]": { "model_profile": "canonical" },
            "[senpi]": { "model_profile": "legacy" }
        }"#,
        Some("native"),
    );
    assert_eq!(result.config["model_profile"], json!("canonical"));
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, "deprecated-keys");
    assert_eq!(
        result.diagnostics[0].issue_paths,
        vec!["[senpi]".to_string()]
    );
    assert!(result.diagnostics[0].message.contains("[native]"));
}

#[test]
fn senpi_caller_applies_the_native_block() {
    let home = tempfile::tempdir().expect("tempdir");
    let result = load_user_config(
        home.path(),
        r#"{ "[native]": { "model_profile": "native-profile" } }"#,
        Some("senpi"),
    );
    assert_eq!(result.config["model_profile"], json!("native-profile"));
}

#[test]
fn memory_write_notice_defaults_enabled() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(value["write_notice"], json!({ "enabled": true }));
}

#[test]
fn memory_write_notice_preserves_an_explicit_false() {
    let value = parsed(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "write_notice": { "enabled": false } }),
    );
    assert_eq!(value["write_notice"]["enabled"], json!(false));
}

#[test]
fn memory_write_notice_rejects_an_unknown_key() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "write_notice": { "bogus": true } })
        )
        .is_err()
    );
}

#[test]
fn memory_write_notice_layer_accepts_a_per_agent_override() {
    let input = json!({ "write_notice": { "enabled": false }, "agents": { "backend-lead": { "write_notice": { "enabled": true } } } });
    assert_eq!(
        parsed(&omo_config_core::omo_memory_settings_layer_schema(), input.clone()),
        input
    );
}

#[test]
fn memory_recall_defaults_apply_when_omitted() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(
        value["recall"],
        json!({
            "enabled": true,
            "max_items": 2,
            "category": "quick",
            "event_caps": { "tool_args": 400, "result_head": 600, "assistant": 1500, "prompt": 4000 },
            "sidecar_max_tokens": 48000,
            "max_concurrent_wakes": 2,
            "tool_budget": 8,
            "query_expansion": false,
        })
    );
}

#[test]
fn memory_recall_empty_block_materializes_nested_defaults() {
    let value = parsed(
        &omo_config_core::omo_memory_settings_schema(),
        json!({ "recall": {} }),
    );
    assert_eq!(value["recall"]["enabled"], json!(true));
    assert_eq!(value["recall"]["max_items"], json!(2));
    assert_eq!(value["recall"]["category"], json!("quick"));
    assert_eq!(
        value["recall"]["event_caps"],
        json!({ "tool_args": 400, "result_head": 600, "assistant": 1500, "prompt": 4000 })
    );
}

#[test]
fn memory_recall_preserves_an_explicit_override() {
    let input = json!({
        "recall": {
            "enabled": false,
            "max_items": 4,
            "category": "deep",
            "event_caps": { "tool_args": 400, "result_head": 600, "assistant": 1500, "prompt": 4000 },
            "sidecar_max_tokens": 48000,
            "max_concurrent_wakes": 2,
            "tool_budget": 8,
            "query_expansion": false,
        },
    });
    assert_eq!(
        parsed(&omo_config_core::omo_memory_settings_schema(), input.clone())["recall"],
        input["recall"]
    );
}

#[test]
fn memory_recall_rejects_max_items_outside_one_through_five() {
    let node = omo_config_core::omo_memory_settings_schema();
    assert!(safe_parse(&node, &json!({ "recall": { "max_items": 0 } })).is_err());
    assert!(safe_parse(&node, &json!({ "recall": { "max_items": 6 } })).is_err());
}

#[test]
fn memory_recall_rejects_invalid_field_types() {
    let node = omo_config_core::omo_memory_settings_schema();
    assert!(safe_parse(&node, &json!({ "recall": { "enabled": "yes" } })).is_err());
    assert!(safe_parse(&node, &json!({ "recall": { "max_items": 2.5 } })).is_err());
}

#[test]
fn memory_recall_rejects_a_mode_field() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "recall": { "mode": "lexical" } })
        )
        .is_err()
    );
}

#[test]
fn memory_recall_rejects_the_removed_knobs() {
    let root = omo_config_core::omo_memory_settings_schema();
    let layer = omo_config_core::omo_memory_settings_layer_schema();
    for recall in [
        json!({ "budget_tokens": 600 }),
        json!({ "excerpt_chars": 200 }),
        json!({ "min_score": 0.1 }),
        json!({ "exclude": ["notes/scratch.md"] }),
    ] {
        assert!(safe_parse(&root, &json!({ "recall": recall.clone() })).is_err());
        assert!(safe_parse(&layer, &json!({ "recall": recall.clone() })).is_err());
        assert!(
            safe_parse(
                &layer,
                &json!({ "agents": { "backend-lead": { "recall": recall.clone() } } })
            )
            .is_err()
        );
    }
}

#[test]
fn memory_recall_rejects_a_malformed_event_cap() {
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "recall": { "event_caps": { "tool_args": -1 } } })
        )
        .is_err()
    );
}

#[test]
fn memory_recall_layer_accepts_a_per_agent_override() {
    let input = json!({ "recall": { "enabled": false }, "agents": { "backend-lead": { "recall": { "max_items": 1 } } } });
    assert_eq!(
        parsed(&omo_config_core::omo_memory_settings_layer_schema(), input.clone()),
        input
    );
}

#[test]
fn memory_recall_query_expansion_defaults_off() {
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), json!({}));
    assert_eq!(value["recall"]["query_expansion"], json!(false));
}

#[test]
fn memory_recall_query_expansion_keeps_root_and_agent_values() {
    let input = json!({ "recall": { "query_expansion": true }, "agents": { "research": { "recall": { "query_expansion": false } } } });
    let value = parsed(&omo_config_core::omo_memory_settings_schema(), input);
    assert_eq!(value["recall"]["query_expansion"], json!(true));
    assert_eq!(
        value["agents"]["research"]["recall"],
        json!({ "query_expansion": false })
    );
}

#[test]
fn memory_recall_query_expansion_rejects_a_non_boolean() {
    let recall = json!({ "query_expansion": "on" });
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_schema(),
            &json!({ "recall": recall.clone() })
        )
        .is_err()
    );
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_recall_layer_schema(),
            &recall
        )
        .is_err()
    );
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_layer_schema(),
            &json!({ "recall": recall.clone() })
        )
        .is_err()
    );
    assert!(
        safe_parse(
            &omo_config_core::omo_memory_settings_layer_schema(),
            &json!({ "agents": { "research": { "recall": recall } } })
        )
        .is_err()
    );
}

fn isolation_defaults() -> Value {
    json!({
        "enabled": false,
        "backend": "auto",
        "apply": true,
        "merge": "patch",
        "commits": "generic",
    })
}

#[test]
fn task_isolation_defaults_disable_isolation() {
    let node = omo_config_core::omo_task_settings_schema();
    assert_eq!(parsed(&node, json!({}))["isolation"], isolation_defaults());
    assert_eq!(
        parsed(&node, json!({ "isolation": {} }))["isolation"],
        isolation_defaults()
    );
}

#[test]
fn task_isolation_layers_preserve_omission() {
    let node = omo_config_core::omo_task_settings_layer_schema();
    assert_eq!(parsed(&node, json!({})), json!({}));
    assert_eq!(
        parsed(&node, json!({ "isolation": { "apply": false } })),
        json!({ "isolation": { "apply": false } })
    );
}

#[test]
fn task_isolation_root_accepts_overrides_and_rejects_an_invalid_backend() {
    let node = omo_config_core::omo_task_settings_schema();
    assert_eq!(
        parsed(
            &node,
            json!({ "isolation": { "enabled": true, "backend": "rcopy", "apply": false, "merge": "branch", "commits": "ai" } })
        )["isolation"],
        json!({ "enabled": true, "backend": "rcopy", "apply": false, "merge": "branch", "commits": "ai" })
    );
    assert!(
        safe_parse(&node, &json!({ "isolation": { "backend": "projfs" } })).is_err()
    );
}

#[test]
fn task_isolation_layer_accepts_overrides_and_rejects_an_invalid_backend() {
    let node = omo_config_core::omo_task_settings_layer_schema();
    assert_eq!(
        parsed(
            &node,
            json!({ "isolation": { "enabled": true, "backend": "rcopy", "apply": false, "merge": "branch", "commits": "ai" } })
        ),
        json!({ "isolation": { "enabled": true, "backend": "rcopy", "apply": false, "merge": "branch", "commits": "ai" } })
    );
    assert!(
        safe_parse(&node, &json!({ "isolation": { "backend": "projfs" } })).is_err()
    );
}
