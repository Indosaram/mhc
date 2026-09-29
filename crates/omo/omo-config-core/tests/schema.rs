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
    let supported = omo_config_core::codegraph_setting_harness_support("codegraph.enabled");
    assert!(supported.is_some());
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
        "codegraph": { "daemon": true },
        "task": {},
        "teams": {
            "builders": {
                "description": "Build team",
                "members": [{ "name": "quick-one", "kind": "category", "category": "quick", "prompt": "Help" }]
            }
        },
    });
    let value = parsed(&omo_config_core::omo_config_schema(), config);
    assert_eq!(value["codegraph"]["daemon"], json!(true));
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
fn config_schema_defaults_the_empty_codegraph_block_on() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "codegraph": {} }),
    );
    assert_eq!(value["codegraph"]["daemon"], json!(true));
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
fn config_schema_reports_the_bad_codegraph_field_path() {
    let reported = issues(
        &omo_config_core::omo_config_schema(),
        json!({ "codegraph": { "daemon": "yes" } }),
    );
    assert!(issue_paths(&reported).contains(&"codegraph.daemon".to_string()));
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
        "[codex]": { "codegraph": { "daemon": false } },
        "profiles": {
            "focused": {
                "categories": { "deep": { "model": "sol" } },
                "models": { "sol": { "model": "openai/gpt-5.6-sol", "reasoningEffort": "high" } },
                "[opencode]": { "background_task": { "enabled": false } },
                "[senpi]": { "task": { "default_concurrency": 2 } },
                "[codex]": { "codegraph": { "enabled": false } },
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
    assert_eq!(value["[codex]"]["codegraph"]["daemon"], json!(false));
    assert_eq!(
        value["profiles"]["focused"]["[codex]"]["codegraph"]["enabled"],
        json!(false)
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
fn unified_config_parses_the_legacy_codegraph_setting_set_with_legacy_defaults() {
    let config = json!({
        "codegraph": {
            "enabled": false,
            "auto_provision": false,
            "daemon": false,
            "telemetry": true,
            "install_dir": "/tmp/omo-codegraph",
            "watch_debounce_ms": 250,
            "excluded_roots": ["/tmp/generated", "/tmp/vendor"],
        }
    });
    let explicit = parsed(&omo_config_core::omo_config_schema(), config.clone());
    let defaults = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "codegraph": {} }),
    );
    assert_eq!(explicit["codegraph"], config["codegraph"]);
    assert_eq!(
        defaults["codegraph"],
        json!({ "enabled": true, "auto_provision": true, "daemon": true, "telemetry": false })
    );
}

#[test]
fn unified_config_keeps_the_empty_codex_block_default_free() {
    let value = parsed(
        &omo_config_core::omo_config_schema(),
        json!({ "codegraph": { "telemetry": true }, "[codex]": { "codegraph": {} } }),
    );
    assert_eq!(value["codegraph"]["telemetry"], json!(true));
    assert!(value["[codex]"]["codegraph"].get("telemetry").is_none());
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
        "sync": { "enabled": true },
        "search": { "enabled": true },
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
        "sync": { "remote": "file:///tmp/memory-mirror.git", "enabled": true },
        "search": { "enabled": false },
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
