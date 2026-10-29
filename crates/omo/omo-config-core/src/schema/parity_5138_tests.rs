use serde_json::json;

use super::*;
use crate::internal::validate::safe_parse;

#[test]
fn permissions_allow_maps_only_for_bash() {
    for key in ["bash", "edit", "webfetch", "task", "doom_loop", "external_directory", "custom"] {
        // Given a command map at each supported or arbitrary permission key.
        let input = json!({ "permission": { (key): { "*": "allow" } } });
        // When the public agent schema validates the permission.
        let parsed = safe_parse(&omo_agent_def_schema(), &input);
        // Then only bash permits a command map.
        assert_eq!(parsed.is_ok(), key == "bash", "{key}");
    }
}

#[test]
fn permissions_preserve_arbitrary_scalar_keys_and_bash_maps() {
    let input = json!({ "permission": { "custom": "ask", "edit": "deny", "bash": { "git *": "allow" } } });
    let parsed = safe_parse(&omo_agent_def_schema(), &input).expect("valid permissions");
    assert_eq!(parsed, input);
}

#[test]
fn side_panel_is_accepted_in_every_config_scope() {
    for schema in [omo_config_schema(), omo_config_layer_schema(), omo_typed_harness_config_schema(), omo_config_profile_schema()] {
        let input = json!({ "side_panel": { "enabled": true, "sections": { "usage": true } } });
        let parsed = safe_parse(&schema, &input).expect("side panel config");
        assert_eq!(parsed["side_panel"]["sections"]["usage"], true);
    }
    let input = json!({ "[native]": { "side_panel": { "enabled": true } }, "profiles": { "work": { "side_panel": { "width": 30 } } } });
    let parsed = safe_parse(&omo_config_schema(), &input).expect("nested panel config");
    assert_eq!(parsed["profiles"]["work"]["side_panel"]["width"], 30);
}

#[test]
fn side_panel_barrel_resolver_defaults_partial_sections() {
    let input = json!({ "side_panel": { "enabled": true, "sections": { "files": false } } });
    let parsed = resolve_omo_side_panel_settings(&input).expect("partial panel");
    assert_eq!(parsed, json!({
        "enabled": true, "width": "26%", "min_columns": 120, "clickable": true,
        "usage_poll_seconds": 150, "sections": { "session": true, "goal": true,
        "context": true, "usage": false, "agents": true, "tools": true, "files": false, "memory": true }
    }));
}

#[test]
fn side_panel_width_and_integer_bounds_match_upstream() {
    for schema in [omo_side_panel_settings_schema(), omo_side_panel_settings_layer_schema()] {
        for width in [json!(24), json!(160), json!("10%"), json!("50%"), json!("26%")] {
            assert!(safe_parse(&schema, &json!({ "width": width })).is_ok());
        }
        for width in [json!(23), json!(161), json!(24.5), json!("9%"), json!("51%"), json!("026%")] {
            assert!(safe_parse(&schema, &json!({ "width": width })).is_err());
        }
        for (key, min, max) in [("min_columns", 60, 400), ("usage_poll_seconds", 60, 3600)] {
            for (value, valid) in [(min - 1, false), (min, true), (max, true), (max + 1, false)] {
                assert_eq!(safe_parse(&schema, &json!({ (key): value })).is_ok(), valid);
            }
        }
        assert!(safe_parse(&schema, &json!({ "sections": { "unknown": true } })).is_err());
        assert!(safe_parse(&schema, &json!({ "unknown": true })).is_err());
    }
}

#[test]
fn side_panel_layers_do_not_materialize_defaults() {
    let input = json!({ "sections": { "usage": true } });
    let parsed = safe_parse(&omo_side_panel_settings_layer_schema(), &input).expect("partial layer");
    assert_eq!(parsed, input);
}

#[test]
fn memory_projection_defaults_and_agent_layers_are_preserved() {
    let input = json!({ "projection": { "max_bytes": 0 }, "agents": { "reviewer": { "projection": { "max_entries_per_directory": 0 } } } });
    let parsed = safe_parse(&omo_memory_settings_schema(), &input).expect("projection settings");
    assert_eq!(parsed["projection"], json!({ "max_entries_per_directory": 40, "max_bytes": 0 }));
    assert_eq!(parsed["agents"], input["agents"]);
    let defaults = safe_parse(&omo_memory_settings_schema(), &json!({})).expect("memory defaults");
    assert_eq!(defaults["projection"], json!({ "max_entries_per_directory": 40, "max_bytes": 24576 }));
}

#[test]
fn memory_projection_layers_accept_zero_but_reject_negative_and_fractional_limits() {
    for schema in [omo_memory_projection_schema(), omo_memory_projection_layer_schema()] {
        for key in ["max_entries_per_directory", "max_bytes"] {
            for (value, valid) in [(json!(0), true), (json!(7), true), (json!(-1), false), (json!(1.5), false)] {
                assert_eq!(safe_parse(&schema, &json!({ (key): value })).is_ok(), valid);
            }
        }
    }
    let input = json!({ "projection": { "max_bytes": 17 }, "agents": { "reviewer": { "projection": {} } } });
    let parsed = safe_parse(&omo_memory_settings_layer_schema(), &input).expect("memory layer");
    assert_eq!(parsed, input);
}

#[test]
fn memory_rejects_retired_tool_exposure_in_full_and_layer_schemas() {
    for schema in [omo_memory_settings_schema(), omo_memory_settings_layer_schema()] {
        for exposure in ["direct", "search"] {
            let input = json!({ "tool_exposure": exposure });
            let issues = safe_parse(&schema, &input).expect_err("removed key");
            assert!(issues.iter().any(|issue| issue.keys.iter().any(|key| key == "tool_exposure")));
        }
        let parsed = safe_parse(&schema, &json!({})).expect("memory settings");
        assert!(parsed.get("tool_exposure").is_none());
    }
}

#[test]
fn task_global_concurrency_uses_schema_and_injected_resolver_defaults() {
    let parsed = safe_parse(&omo_task_settings_schema(), &json!({})).expect("task defaults");
    assert_eq!(parsed["global_concurrency"], 8);
    for (parallelism, expected) in [(1, 8), (12, 24)] {
        let resolved = resolve_omo_task_settings(&json!({ "global_concurrency": null }), || parallelism).expect("resolved task");
        assert_eq!(resolved["global_concurrency"], expected);
    }
    let resolved = resolve_omo_task_settings(&json!({}), || 6).expect("missing concurrency");
    assert_eq!(resolved["global_concurrency"], 12);
}

#[test]
fn task_residency_is_unlimited_for_missing_and_nullish_resolver_values() {
    let parsed = safe_parse(&omo_task_settings_schema(), &json!({})).expect("task defaults");
    assert_eq!(parsed["residency_max_children"], "unlimited");
    for input in [json!({}), json!({ "residency_max_children": null })] {
        let resolved = resolve_omo_task_settings(&input, || 100).expect("resolved task");
        assert_eq!(resolved["residency_max_children"], "unlimited");
    }
}

#[test]
fn task_zero_caps_remain_unlimited_and_negative_caps_are_invalid() {
    let input = json!({ "default_concurrency": 0, "global_concurrency": 0, "provider_concurrency": { "provider": 0 }, "model_concurrency": { "model": 0 }, "residency_max_children": 0 });
    for schema in [omo_task_settings_schema(), omo_task_settings_layer_schema()] {
        let parsed = safe_parse(&schema, &input).expect("zero caps");
        for (key, value) in input.as_object().expect("object") {
            assert_eq!(&parsed[key], value);
        }
        for key in ["default_concurrency", "global_concurrency", "residency_max_children"] {
            assert!(safe_parse(&schema, &json!({ (key): -1 })).is_err());
        }
        for key in ["provider_concurrency", "model_concurrency"] {
            assert!(safe_parse(&schema, &json!({ (key): { "entry": -1 } })).is_err());
        }
    }
    let resolved = resolve_omo_task_settings(&input, || panic!("explicit global cap must bypass parallelism")).expect("explicit caps");
    assert_eq!(resolved["global_concurrency"], 0);
    assert_eq!(resolved["residency_max_children"], 0);
}

#[test]
fn task_resident_idle_timeout_obeys_positive_safe_integer_bounds() {
    let defaults = safe_parse(&omo_task_settings_schema(), &json!({})).expect("task defaults");
    assert_eq!(defaults["resident_idle_timeout_ms"], 900000);
    for schema in [omo_task_settings_schema(), omo_task_settings_layer_schema()] {
        for (value, valid) in [(json!(0), false), (json!(-1), false), (json!(1.5), false), (json!(1), true), (json!(9_007_199_254_740_991_i64), true), (json!(9_007_199_254_740_992_i64), false)] {
            assert_eq!(safe_parse(&schema, &json!({ "resident_idle_timeout_ms": value })).is_ok(), valid);
        }
    }
}

#[test]
fn team_subagent_allows_empty_prompt_without_relaxing_category_prompt() {
    let input = json!({ "name": "reviewer", "kind": "subagent_type", "subagent_type": "explore", "prompt": "" });
    let parsed = safe_parse(&omo_team_subagent_member_schema(), &input).expect("empty subagent prompt");
    assert_eq!(parsed["prompt"], "");
    assert!(safe_parse(&omo_team_category_member_schema(), &json!({ "name": "builder", "kind": "category", "category": "quick", "prompt": "" })).is_err());
}

#[test]
fn telemetry_support_routes_enabled_setting_to_native_harness() {
    let supported = TELEMETRY_HARNESS_SUPPORT.iter().find(|(path, _)| *path == "telemetry.enabled").map(|(_, harnesses)| *harnesses);
    assert_eq!(supported, Some(["native"].as_slice()));
}
