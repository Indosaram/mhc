use pretty_assertions::assert_eq;

use super::*;
use crate::state::ResolvedModelSource;

fn resolved(
    provider: &str,
    model_id: &str,
    display: &str,
    source: ResolvedModelSource,
) -> ResolvedModelRecord {
    ResolvedModelRecord {
        display: display.to_string(),
        ..ResolvedModelRecord::new(source, provider, model_id)
    }
}

fn identity(input: TaskIdentityInput<'_>) -> String {
    task_identity_label(&input)
}

fn target(input: StatusTargetInput<'_>) -> Option<String> {
    format_status_target(&input)
}

#[test]
fn task_summary_wins_over_description_name_and_id() {
    assert_eq!(
        identity(TaskIdentityInput {
            task_id: "st_00000001",
            name: Some("task-1"),
            description: Some("quick label"),
            task_summary: Some("Refactor auth into sessions"),
        }),
        "Refactor auth into sessions"
    );
}

#[test]
fn blank_task_summary_falls_back_to_description() {
    assert_eq!(
        identity(TaskIdentityInput {
            task_id: "st_00000001",
            description: Some("Audit renderers"),
            task_summary: Some("  "),
            ..TaskIdentityInput::default()
        }),
        "Audit renderers"
    );
}

#[test]
fn description_wins_over_name_and_id() {
    assert_eq!(
        identity(TaskIdentityInput {
            task_id: "st_00000001",
            name: Some("task-1"),
            description: Some("Audit renderers"),
            ..TaskIdentityInput::default()
        }),
        "Audit renderers"
    );
}

#[test]
fn stable_name_is_used_without_description() {
    assert_eq!(
        identity(TaskIdentityInput {
            task_id: "st_00000001",
            name: Some("reviewer"),
            ..TaskIdentityInput::default()
        }),
        "reviewer"
    );
}

#[test]
fn id_is_last_resort_handle() {
    assert_eq!(
        identity(TaskIdentityInput {
            task_id: "st_00000001",
            ..TaskIdentityInput::default()
        }),
        "st_00000001"
    );
}

#[test]
fn whitespace_only_labels_are_ignored() {
    assert_eq!(
        identity(TaskIdentityInput {
            task_id: "st_00000001",
            name: Some("  "),
            description: Some("\n"),
            ..TaskIdentityInput::default()
        }),
        "st_00000001"
    );
}

#[test]
fn overlong_description_is_excerpted() {
    let description = "x".repeat(80);
    let label = identity(TaskIdentityInput {
        task_id: "st_00000001",
        description: Some(&description),
        ..TaskIdentityInput::default()
    });
    assert!(label.chars().count() <= 48);
    assert!(label.ends_with("..."));
}

#[test]
fn category_with_resolved_model_is_qualified() {
    let model = ResolvedModelRecord {
        reasoning_effort: Some("high".to_string()),
        ..resolved(
            "quotio-openai",
            "gpt-5.6-luna-fast",
            "gpt-5.6-luna-fast",
            ResolvedModelSource::Category,
        )
    };
    assert_eq!(
        target(StatusTargetInput {
            category: Some("quick"),
            resolved_model: Some(&model),
            ..StatusTargetInput::default()
        }),
        Some("category:quick(quotio-openai/gpt-5.6-luna-fast:high)".to_string())
    );
}

#[test]
fn agent_type_alone_shares_category_grammar() {
    assert_eq!(
        target(StatusTargetInput {
            agent_type: Some("momus"),
            ..StatusTargetInput::default()
        }),
        Some("agent:momus".to_string())
    );
}

#[test]
fn agent_type_with_resolved_model_is_qualified_like_category() {
    let model = ResolvedModelRecord {
        reasoning: Some("high".to_string()),
        ..resolved(
            "openai",
            "gpt-5.6-sol-fast",
            "gpt-5.6-sol-fast",
            ResolvedModelSource::Agent,
        )
    };
    assert_eq!(
        target(StatusTargetInput {
            agent_type: Some("momus"),
            resolved_model: Some(&model),
            ..StatusTargetInput::default()
        }),
        Some("agent:momus(openai/gpt-5.6-sol-fast:high)".to_string())
    );
}

#[test]
fn agent_type_with_raw_model_is_qualified() {
    assert_eq!(
        target(StatusTargetInput {
            agent_type: Some("explore"),
            model: Some("anthropic/claude-sonnet-4-6"),
            ..StatusTargetInput::default()
        }),
        Some("agent:explore(anthropic/claude-sonnet-4-6)".to_string())
    );
}

fn sol(reasoning_effort: Option<&str>, variant: Option<&str>) -> ResolvedModelRecord {
    ResolvedModelRecord {
        reasoning_effort: reasoning_effort.map(str::to_string),
        variant: variant.map(str::to_string),
        ..resolved(
            "openai",
            "gpt-5.6-sol",
            "GPT-5.6 Sol",
            ResolvedModelSource::Category,
        )
    }
}

fn ultrabrain_target(model: &ResolvedModelRecord) -> Option<String> {
    target(StatusTargetInput {
        category: Some("ultrabrain"),
        resolved_model: Some(model),
        ..StatusTargetInput::default()
    })
}

#[test]
fn reasoning_effort_wins_over_variant() {
    assert_eq!(
        ultrabrain_target(&sol(Some("xhigh"), Some("sol"))),
        Some("category:ultrabrain(openai/gpt-5.6-sol:xhigh)".to_string())
    );
}

#[test]
fn variant_only_is_rendered() {
    assert_eq!(
        ultrabrain_target(&sol(None, Some("sol"))),
        Some("category:ultrabrain(openai/gpt-5.6-sol:sol)".to_string())
    );
}

#[test]
fn reasoning_effort_only_is_rendered() {
    assert_eq!(
        ultrabrain_target(&sol(Some("xhigh"), None)),
        Some("category:ultrabrain(openai/gpt-5.6-sol:xhigh)".to_string())
    );
}

#[test]
fn no_effort_or_variant_renders_bare_model() {
    assert_eq!(
        ultrabrain_target(&sol(None, None)),
        Some("category:ultrabrain(openai/gpt-5.6-sol)".to_string())
    );
}

#[test]
fn terminal_controls_in_model_metadata_are_normalized() {
    let model = ResolvedModelRecord {
        display: "GPT\u{1b}]0;hidden\u{7}-5.6 Sol".to_string(),
        ..sol(Some("xhigh\u{7}"), Some("sol\u{7f}"))
    };
    assert_eq!(
        target(StatusTargetInput {
            category: Some("quick"),
            resolved_model: Some(&model),
            ..StatusTargetInput::default()
        }),
        Some("category:quick(openai/gpt-5.6-sol:xhigh)".to_string())
    );
}

#[test]
fn raw_model_is_sanitized_and_qualifies_target() {
    assert_eq!(
        target(StatusTargetInput {
            category: Some("quick"),
            model: Some("anthropic/claude-sonnet-4-5"),
            ..StatusTargetInput::default()
        }),
        Some("category:quick(anthropic/claude-sonnet-4-5)".to_string())
    );
    assert_eq!(
        target(StatusTargetInput {
            model: Some("anthropic/claude-sonnet-4-5"),
            ..StatusTargetInput::default()
        }),
        Some("model:anthropic/claude-sonnet-4-5".to_string())
    );
    assert_eq!(
        target(StatusTargetInput {
            category: Some("quick"),
            model: Some("raw\u{1b}[31m-model"),
            ..StatusTargetInput::default()
        }),
        Some("category:quick(raw-model)".to_string())
    );
}

#[test]
fn no_target_facts_emit_nothing() {
    assert_eq!(target(StatusTargetInput::default()), None);
}

#[test]
fn full_live_facts_follow_canonical_grammar() {
    let stats = StatusLineStats {
        runtime_ms: Some(1_000),
        turns: 3,
        tool_calls: 7,
        tokens_per_second: Some(62.0),
        ..StatusLineStats::default()
    };
    assert_eq!(
        compose_status_line(&StatusLineInput {
            identity: "Audit renderers",
            target: Some("quick (kimi-coding/kimi-k3:max)"),
            stats: Some(&stats),
            verb: Some("running read src/foo.ts"),
        }),
        "Audit renderers · quick (kimi-coding/kimi-k3:max) · turn 3 (7 tools) · running read src/foo.ts · 62 tok/s"
    );
}

#[test]
fn only_cost_sits_before_tps() {
    let stats = StatusLineStats {
        runtime_ms: Some(1_000),
        turns: 3,
        tool_calls: 7,
        tokens_per_second: Some(62.0),
        cost_usd: Some(0.4213),
        cache_hit_rate_last: Some(0.8712),
        cache_hit_rate_run: Some(0.4),
    };
    assert_eq!(
        compose_status_line(&StatusLineInput {
            identity: "Audit renderers",
            target: Some("quick (kimi-coding/kimi-k3:max)"),
            stats: Some(&stats),
            verb: Some("running read src/foo.ts"),
        }),
        "Audit renderers · quick (kimi-coding/kimi-k3:max) · turn 3 (7 tools) · running read src/foo.ts · $0.4213 · 62 tok/s"
    );
}

#[test]
fn cache_rate_without_cost_renders_no_spend() {
    let stats = StatusLineStats {
        runtime_ms: Some(0),
        turns: 1,
        tool_calls: 0,
        tokens_per_second: Some(8.0),
        cache_hit_rate_last: Some(0.5),
        cache_hit_rate_run: Some(0.1),
        ..StatusLineStats::default()
    };
    assert_eq!(
        compose_status_line(&StatusLineInput {
            identity: "t",
            stats: Some(&stats),
            ..StatusLineInput::default()
        }),
        "t · turn 1 · 8 tok/s"
    );
}

#[test]
fn single_tool_call_is_singular() {
    let stats = StatusLineStats {
        runtime_ms: Some(0),
        turns: 1,
        tool_calls: 1,
        ..StatusLineStats::default()
    };
    assert_eq!(
        compose_status_line(&StatusLineInput {
            identity: "t",
            stats: Some(&stats),
            verb: Some("running"),
            ..StatusLineInput::default()
        }),
        "t · turn 1 (1 tool) · running"
    );
}

#[test]
fn no_stats_emits_only_known_tokens() {
    assert_eq!(
        compose_status_line(&StatusLineInput {
            identity: "t",
            verb: Some("waiting (running)"),
            ..StatusLineInput::default()
        }),
        "t · waiting (running)"
    );
}
