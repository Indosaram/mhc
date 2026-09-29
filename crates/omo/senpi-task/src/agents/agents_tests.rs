use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use super::*;
use crate::delegate_adapter::DelegateFallbackEntry;
use crate::host::SenpiModelRegistry;
use crate::host::fake::{catalog_registry, model, registry};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

fn rules_of(input: Value) -> Vec<AgentToolRule> {
    normalize_tool_rules(Some(&input)).unwrap_or_default()
}

fn roster(definitions: Vec<AgentDefinition>) -> Vec<(String, AgentDefinition)> {
    definitions
        .into_iter()
        .map(|definition| (definition.name.clone(), definition))
        .collect()
}

fn resolve(
    name: &str,
    agents: &[(String, AgentDefinition)],
    models: &dyn SenpiModelRegistry,
) -> AgentResolutionResult {
    resolve_agent(name, agents, Some(models), &ResolveAgentOptions::default())
}

fn expect_resolved(result: AgentResolutionResult) -> ResolvedAgent {
    match result {
        AgentResolutionResult::Resolved(resolved) => *resolved,
        other => panic!("Expected resolved agent, got {other:?}"),
    }
}

fn candidate(model: &str) -> AgentModelCandidate {
    AgentModelCandidate {
        model: model.to_string(),
        ..AgentModelCandidate::default()
    }
}

fn candidate_entry(candidate: AgentModelCandidate) -> Option<Vec<AgentModelEntry>> {
    Some(vec![AgentModelEntry::Candidate(candidate)])
}

fn text(value: &str) -> Option<String> {
    Some(value.to_string())
}

// ---- agents/tools.test.ts ----

#[test]
fn tools_string_action_record_maps_allow_and_deny() {
    let rules = rules_of(json!({ "read": "allow", "write": "deny" }));
    assert_eq!(resolve_tool_rule(&rules, "read"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "write"), Some(false));
    assert_eq!(resolve_tool_rule(&rules, "other"), None);
}

#[test]
fn tools_nested_command_map_last_match_wins() {
    let rules = rules_of(json!({ "bash": { "*": "deny", "rg *": "allow" } }));
    assert_eq!(resolve_tool_rule(&rules, "bash rg foo"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "bash ls"), Some(false));
}

#[test]
fn tools_reversed_nested_map_trailing_deny_wins() {
    let rules = rules_of(json!({ "bash": { "rg *": "allow", "*": "deny" } }));
    assert_eq!(resolve_tool_rule(&rules, "bash rg foo"), Some(false));
    assert_eq!(resolve_tool_rule(&rules, "bash ls"), Some(false));
}

#[test]
fn tools_mixed_record_every_shape_evaluates() {
    let rules = rules_of(json!({ "read": true, "write": "deny", "bash": { "rg *": "allow" } }));
    assert_eq!(resolve_tool_rule(&rules, "read"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "write"), Some(false));
    assert_eq!(resolve_tool_rule(&rules, "bash rg foo"), Some(true));
}

#[test]
fn tools_ask_action_is_non_grant_overriding_broader_allow() {
    let standalone = rules_of(json!({ "danger": "ask" }));
    let scoped = rules_of(json!({ "bash": { "*": "allow", "danger*": "ask" } }));
    assert_eq!(resolve_tool_rule(&standalone, "danger"), Some(false));
    assert_eq!(resolve_tool_rule(&scoped, "bash safe"), Some(true));
    assert_eq!(resolve_tool_rule(&scoped, "bash danger-op"), Some(false));
}

#[test]
fn tools_boolean_record_keeps_working() {
    let rules = rules_of(json!({ "read": true, "write": false }));
    assert_eq!(resolve_tool_rule(&rules, "read"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "write"), Some(false));
}

#[test]
fn tools_array_shape_bang_prefix_denies() {
    let rules = rules_of(json!(["shell", "!danger"]));
    assert_eq!(resolve_tool_rule(&rules, "shell"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "danger"), Some(false));
}

// ---- agents/interaction-policy.test.ts ----

#[test]
fn interaction_policy_momus_is_one_shot_plan_review() {
    let (name, policy) = AGENT_INTERACTION_POLICIES[0];
    assert_eq!(name, "momus");
    assert!(policy.one_shot);
    assert_eq!(policy.prompt_contract, "plan-review");
}

#[test]
fn interaction_policy_only_momus_present() {
    let keys: Vec<&str> = AGENT_INTERACTION_POLICIES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(keys, vec!["momus"]);
    assert!(!keys.contains(&"metis"));
}

#[test]
fn one_shot_names_contain_exactly_momus() {
    let names = one_shot_agent_names();
    assert!(names.contains("momus"));
    for absent in ["metis", "explore", "librarian"] {
        assert!(!names.contains(absent));
    }
    assert_eq!(names.len(), 1);
}

#[test]
fn interaction_policy_lookup_momus() {
    let policy = interaction_policy_for_agent("momus").expect("policy");
    assert!(policy.one_shot);
    assert_eq!(policy.prompt_contract, "plan-review");
    assert!(!policy.send_denial_reminder.is_empty());
}

#[test]
fn interaction_policy_lookup_unknown_is_none() {
    assert_eq!(interaction_policy_for_agent("explore"), None);
    assert_eq!(interaction_policy_for_agent("sisyphus"), None);
}

#[test]
fn interaction_policy_lookup_metis_is_none() {
    assert_eq!(interaction_policy_for_agent("metis"), None);
}

#[test]
fn momus_denial_reminder_wrapped_in_sentinel_tags() {
    let reminder = AGENT_INTERACTION_POLICIES[0].1.send_denial_reminder;
    assert!(!reminder.is_empty());
    assert!(reminder.starts_with("<system-reminder>"));
    assert!(reminder.ends_with("</system-reminder>"));
}

// ---- agents/invocation-guard.test.ts ----

#[derive(Default)]
struct StateOf {
    invoked: Vec<&'static str>,
    requested: Vec<&'static str>,
    artifact: bool,
    references: Vec<PlanArtifactReference>,
}

impl SkillInvocationState for StateOf {
    fn has_invoked(&self, skill: &str) -> bool {
        self.invoked.contains(&skill)
    }

    fn has_user_requested(&self, skill: &str) -> bool {
        self.requested.contains(&skill)
    }

    fn has_plan_artifact(&self) -> bool {
        self.artifact
    }

    fn plan_artifact_references(&self) -> Vec<PlanArtifactReference> {
        self.references.clone()
    }
}

fn deny_message(verdict: InvocationGuardVerdict) -> String {
    match verdict {
        InvocationGuardVerdict::Deny { message } => message,
        InvocationGuardVerdict::Allow => panic!("expected deny"),
    }
}

#[test]
fn invocation_conditions_metis_and_momus_are_plan_gated() {
    let gated = plan_gated_agent_names();
    assert!(gated.contains("metis") && gated.contains("momus"));
    assert!(!gated.contains("explore") && !gated.contains("librarian"));
    for name in ["metis", "momus"] {
        let condition = invocation_condition_for_agent(name).expect("condition");
        assert_eq!(condition.requires_skills, ["ulw-plan"]);
        assert!(condition.requires_plan_artifact);
        assert_eq!(condition.forbids_skills, ["start-work"]);
    }
    assert_eq!(AGENT_INVOCATION_CONDITIONS.len(), 2);
}

#[test]
fn invocation_condition_absent_for_non_gated_agent() {
    assert_eq!(invocation_condition_for_agent("explore"), None);
    assert_eq!(invocation_condition_for_agent("sisyphus"), None);
}

#[test]
fn guard_allows_non_gated_agent() {
    assert_eq!(
        evaluate_invocation_guard("explore", &StateOf::default()),
        InvocationGuardVerdict::Allow
    );
}

#[test]
fn guard_denies_momus_in_empty_session_naming_ulw_plan() {
    let message = deny_message(evaluate_invocation_guard("momus", &StateOf::default()));
    assert!(message.contains("momus"));
    assert!(message.contains("ulw-plan"));
}

#[test]
fn guard_denies_metis_in_empty_session_naming_ulw_plan() {
    let message = deny_message(evaluate_invocation_guard("metis", &StateOf::default()));
    assert!(message.contains("ulw-plan"));
}

#[test]
fn guard_denies_model_invocation_without_user_request() {
    let state = StateOf {
        invoked: vec!["ulw-plan"],
        artifact: true,
        ..StateOf::default()
    };
    assert!(matches!(
        evaluate_invocation_guard("momus", &state),
        InvocationGuardVerdict::Deny { .. }
    ));
}

#[test]
fn guard_denies_user_request_without_artifact() {
    let state = StateOf {
        requested: vec!["ulw-plan"],
        ..StateOf::default()
    };
    assert!(deny_message(evaluate_invocation_guard("momus", &state)).contains(".omo/plans"));
}

#[test]
fn guard_allows_user_request_with_artifact() {
    let state = StateOf {
        requested: vec!["ulw-plan"],
        artifact: true,
        ..StateOf::default()
    };
    assert_eq!(
        evaluate_invocation_guard("momus", &state),
        InvocationGuardVerdict::Allow
    );
}

#[test]
fn guard_denies_after_start_work() {
    let state = StateOf {
        requested: vec!["ulw-plan"],
        artifact: true,
        invoked: vec!["start-work"],
        ..StateOf::default()
    };
    assert!(deny_message(evaluate_invocation_guard("momus", &state)).contains("start-work"));
}

#[test]
fn guard_forbidden_denial_precedes_missing_requirement() {
    let state = StateOf {
        invoked: vec!["start-work"],
        ..StateOf::default()
    };
    assert!(deny_message(evaluate_invocation_guard("momus", &state)).contains("start-work"));
}

#[test]
fn empty_skill_invocations_report_nothing() {
    assert_eq!(EmptySkillInvocations.plan_artifact_references(), vec![]);
    assert!(!EmptySkillInvocations.has_plan_artifact());
}

#[test]
fn state_plan_references_are_returned() {
    let references = vec![
        PlanArtifactReference {
            path: "/repo/.omo/plans/alpha.md".to_string(),
            count: 3,
            last_touched_at: 7,
        },
        PlanArtifactReference {
            path: "/repo/.omo/plans/beta.md".to_string(),
            count: 1,
            last_touched_at: 9,
        },
    ];
    let state = StateOf {
        artifact: true,
        references: references.clone(),
        ..StateOf::default()
    };
    assert_eq!(state.plan_artifact_references(), references);
}

#[test]
fn guard_missing_user_request_names_user_driven_unlock() {
    let state = StateOf {
        artifact: true,
        ..StateOf::default()
    };
    let message = deny_message(evaluate_invocation_guard("momus", &state));
    assert!(message.contains("/skill:ulw-plan"));
    assert!(message.to_lowercase().contains("ask the user"));
}

#[test]
fn guard_missing_artifact_names_plan_file_requirement() {
    let state = StateOf {
        requested: vec!["ulw-plan"],
        ..StateOf::default()
    };
    assert!(deny_message(evaluate_invocation_guard("metis", &state)).contains(".omo/plans"));
}

#[test]
fn guard_terminal_denial_does_not_advertise_unlock() {
    let state = StateOf {
        invoked: vec!["start-work"],
        requested: vec!["ulw-plan"],
        artifact: true,
        ..StateOf::default()
    };
    let message = deny_message(evaluate_invocation_guard("momus", &state));
    assert!(message.contains("start-work"));
    assert!(!message.contains("/skill:ulw-plan"));
}

// ---- agents/builtin/builtin-agents.test.ts ----

const CURATED_AGENT_NAMES: [&str; 4] = ["explore", "librarian", "metis", "momus"];

#[test]
fn builtin_defaults_are_the_four_curated_agents() {
    let mut names: Vec<&str> = BUILTIN_AGENT_DEFAULTS
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, CURATED_AGENT_NAMES);
}

#[test]
fn builtin_record_maps_names_to_definitions() {
    for name in CURATED_AGENT_NAMES {
        assert_eq!(
            builtin_agent(name).map(|definition| definition.name.as_str()),
            Some(name)
        );
    }
    assert_eq!(builtin_agent("sisyphus"), None);
}

#[test]
fn curated_name_set_has_exactly_four_names() {
    let names = curated_readonly_agent_names();
    assert_eq!(names.len(), 4);
    for name in CURATED_AGENT_NAMES {
        assert!(names.contains(name));
    }
}

#[test]
fn builtin_definitions_are_in_process_subagents() {
    for definition in BUILTIN_AGENT_DEFAULTS.iter() {
        assert_eq!(definition.mode.as_deref(), Some("subagent"));
        assert_eq!(definition.execution_mode.as_deref(), Some("in-process"));
    }
}

#[test]
fn builtin_definitions_carry_the_nine_literal_allow_rules() {
    let mut expected = vec![
        "read",
        "find",
        "grep",
        "ls",
        "bash",
        "lsp_diagnostics",
        "lsp_goto_definition",
        "lsp_find_references",
        "lsp_symbols",
    ];
    expected.sort_unstable();
    for definition in BUILTIN_AGENT_DEFAULTS.iter() {
        let rules = definition.tools.as_deref().unwrap_or_default();
        assert_eq!(rules.len(), 9);
        let mut patterns: Vec<&str> = rules.iter().map(|rule| rule.pattern.as_str()).collect();
        patterns.sort_unstable();
        assert_eq!(patterns, expected);
        assert!(rules.iter().all(|rule| rule.allow));
    }
}

#[test]
fn builtin_descriptions_are_non_empty_and_unbranded() {
    for definition in BUILTIN_AGENT_DEFAULTS.iter() {
        let description = definition.description.as_deref().expect("description");
        assert!(!description.is_empty());
        assert!(!description.contains("OhMyOpenCode"));
        assert!(!description.contains("OhMyOpenAgent"));
    }
}

// ---- agents/builtin/fallback-chains.test.ts ----

fn entry(providers: &[&str], model: &str, variant: Option<&str>) -> DelegateFallbackEntry {
    DelegateFallbackEntry::new(providers, model, variant)
}

#[test]
fn fallback_chain_keys_are_the_curated_agents() {
    let mut keys: Vec<&str> = AGENT_FALLBACK_CHAINS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, CURATED_AGENT_NAMES);
}

#[test]
fn fallback_chain_entries_have_providers_and_model() {
    for name in CURATED_AGENT_NAMES {
        let chain = agent_fallback_chain(name).expect("chain");
        assert!(!chain.is_empty());
        for rung in chain {
            assert!(!rung.providers.is_empty());
            assert!(!rung.model.is_empty());
        }
    }
}

#[test]
fn fallback_chain_lengths_match_transcription() {
    let lengths: Vec<(&str, usize)> = CURATED_AGENT_NAMES
        .iter()
        .map(|name| (*name, agent_fallback_chain(name).map_or(0, <[_]>::len)))
        .collect();
    assert_eq!(
        lengths,
        vec![("explore", 9), ("librarian", 9), ("metis", 5), ("momus", 7)]
    );
}

#[test]
fn fallback_chain_table_is_pinned() {
    let fast = vec![
        entry(&["openai"], "gpt-5.6-luna-fast", Some("low")),
        entry(&["deepseek"], "deepseek-v4-flash", Some("max")),
        entry(
            &["opencode-go", "bailian-coding-plan"],
            "qwen3.5-plus",
            None,
        ),
        entry(&["vercel"], "minimax-m2.7-highspeed", None),
        entry(&["opencode-go", "vercel"], "minimax-m3", None),
        entry(
            &["minimax-coding-plan", "minimax-cn-coding-plan"],
            "MiniMax-M3",
            None,
        ),
        entry(&["opencode-go", "vercel"], "minimax-m2.7", None),
        entry(
            &["anthropic", "github-copilot", "vercel"],
            "claude-haiku-4-5",
            None,
        ),
        entry(&["openai", "vercel"], "gpt-5.4-nano", None),
    ];
    let claude = ["anthropic", "github-copilot", "opencode", "vercel"];
    let expected = vec![
        ("explore", fast.clone()),
        ("librarian", fast),
        (
            "metis",
            vec![
                entry(&claude, "claude-sonnet-4-6", None),
                entry(&claude, "claude-opus-5", Some("max")),
                entry(
                    &["openai", "github-copilot", "opencode", "vercel"],
                    "gpt-5.6-sol",
                    Some("medium"),
                ),
                entry(&["opencode-go", "vercel"], "glm-5.2", None),
                entry(&["kimi-for-coding"], "kimi-k3", None),
            ],
        ),
        (
            "momus",
            vec![
                entry(&["openai", "vercel"], "gpt-5.6-terra", Some("high")),
                entry(&["github-copilot"], "gpt-5.6-terra", Some("high")),
                entry(
                    &["openai", "opencode", "vercel"],
                    "gpt-5.6-sol",
                    Some("xhigh"),
                ),
                entry(&["github-copilot"], "gpt-5.6-sol", Some("high")),
                entry(&claude, "claude-opus-5", Some("max")),
                entry(
                    &["google", "github-copilot", "opencode", "vercel"],
                    "gemini-3.1-pro",
                    Some("high"),
                ),
                entry(&["opencode-go", "vercel"], "glm-5.2", None),
            ],
        ),
    ];
    assert_eq!(*AGENT_FALLBACK_CHAINS, expected);
}

// ---- luna-deepseek-chain-policy.test.ts ----

#[test]
fn quick_places_non_reasoning_deepseek_after_luna() {
    let quick = crate::category::category_fallback_chain("quick").expect("quick chain");
    assert_eq!(
        quick[1..3].to_vec(),
        vec![
            entry(&["openai-codex"], "gpt-5.6-luna-fast", Some("low")),
            entry(&["deepseek"], "deepseek-v4-flash", Some("off")),
        ]
    );
}

#[test]
fn explore_and_librarian_place_max_deepseek_after_luna() {
    for agent in ["explore", "librarian"] {
        let chain = agent_fallback_chain(agent).expect("chain");
        assert_eq!(
            chain[0..2].to_vec(),
            vec![
                entry(&["openai"], "gpt-5.6-luna-fast", Some("low")),
                entry(&["deepseek"], "deepseek-v4-flash", Some("max")),
            ],
            "{agent}"
        );
    }
}

// ---- agents/omo-config-agents.test.ts ----

#[test]
fn omo_config_full_field_set_maps_to_definition() {
    let agents = map_omo_config_agents(&json!({ "agents": { "reviewer": {
        "description": "Reviews diffs",
        "prompt": "You are a reviewer.",
        "model": "openai/gpt-5",
        "models": ["openai/gpt-5", "anthropic/claude"],
        "execution_mode": "process",
        "background": true,
        "max_depth": 2,
        "allowed_subagents": ["quick"],
        "temperature": 0.4,
        "disable": false,
    } } }));
    assert_eq!(
        agents,
        vec![(
            "reviewer".to_string(),
            AgentDefinition {
                description: text("Reviews diffs"),
                prompt: text("You are a reviewer."),
                model: text("openai/gpt-5"),
                models: Some(vec![
                    AgentModelEntry::Model("openai/gpt-5".to_string()),
                    AgentModelEntry::Model("anthropic/claude".to_string()),
                ]),
                execution_mode: text("process"),
                background: Some(true),
                max_depth: Some(2),
                allowed_subagents: Some(vec!["quick".to_string()]),
                temperature: Some(0.4),
                disable: Some(false),
                ..AgentDefinition::named("reviewer")
            }
        )]
    );
}

#[test]
fn omo_config_tools_record_becomes_rules() {
    let agents = map_omo_config_agents(
        &json!({ "agents": { "builder": { "tools": { "read": true, "bash": false } } } }),
    );
    let builder = &agents[0].1;
    assert_eq!(builder.name, "builder");
    assert_eq!(
        builder.tools,
        Some(vec![
            AgentToolRule::new("read", true),
            AgentToolRule::new("bash", false)
        ])
    );
}

#[test]
fn omo_config_minimal_agent_keeps_optional_keys_absent() {
    let agents = map_omo_config_agents(&json!({ "agents": { "minimal": {} } }));
    assert_eq!(
        agents,
        vec![("minimal".to_string(), AgentDefinition::named("minimal"))]
    );
}

#[test]
fn omo_config_without_agents_is_empty() {
    assert_eq!(map_omo_config_agents(&json!({})), vec![]);
}

// ---- agents/agent-model-effort.test.ts ----

fn first_candidate(agents: &[(String, AgentDefinition)]) -> AgentModelCandidate {
    match agents[0].1.models.as_deref().and_then(<[_]>::first) {
        Some(AgentModelEntry::Candidate(candidate)) => candidate.clone(),
        other => panic!("Expected the bridged entry to stay an object, got {other:?}"),
    }
}

#[test]
fn effort_omo_entry_objects_survive_bridge() {
    let agents = map_omo_config_agents(&json!({ "agents": { "explore": {
        "models": [{ "model": "quotio-openai/gpt-5.6-luna-fast", "reasoningEffort": "minimal" }]
    } } }));
    let entry = first_candidate(&agents);
    assert_eq!(entry.model, "quotio-openai/gpt-5.6-luna-fast");
    assert_eq!(entry.reasoning_effort.as_deref(), Some("minimal"));
}

#[test]
fn effort_omo_entry_canonical_reasoning_preserved() {
    let agents = map_omo_config_agents(&json!({ "agents": { "explore": {
        "models": [{ "model": "provider/model", "reasoning": "high" }]
    } } }));
    assert_eq!(first_candidate(&agents).reasoning.as_deref(), Some("high"));
}

#[test]
fn effort_omo_plain_string_models_preserved() {
    let agents = map_omo_config_agents(
        &json!({ "agents": { "explore": { "models": ["openai/a", "openai/b"] } } }),
    );
    assert_eq!(
        agents[0].1.models,
        Some(vec![
            AgentModelEntry::Model("openai/a".to_string()),
            AgentModelEntry::Model("openai/b".to_string()),
        ])
    );
}

#[test]
fn effort_canonical_reasoning_reaches_resolved_record() {
    let agents = roster(vec![AgentDefinition {
        models: candidate_entry(AgentModelCandidate {
            reasoning: text("high"),
            ..candidate("quotio-openai/gpt-5.6-luna-fast")
        }),
        ..AgentDefinition::named("explore")
    }]);
    let models = registry(vec![model("quotio-openai", "gpt-5.6-luna-fast")]);
    let result = expect_resolved(resolve("explore", &agents, &models));
    assert_eq!(
        result
            .resolved_model
            .and_then(|record| record.reasoning_effort)
            .as_deref(),
        Some("high")
    );
}

#[test]
fn effort_entry_effort_reaches_resolved_record() {
    let agents = roster(vec![AgentDefinition {
        models: candidate_entry(AgentModelCandidate {
            reasoning_effort: text("minimal"),
            ..candidate("quotio-openai/gpt-5.6-luna-fast")
        }),
        ..AgentDefinition::named("explore")
    }]);
    let models = registry(vec![model("quotio-openai", "gpt-5.6-luna-fast")]);
    let result = expect_resolved(resolve("explore", &agents, &models));
    assert_eq!(result.model, "quotio-openai/gpt-5.6-luna-fast");
    assert_eq!(
        result
            .resolved_model
            .and_then(|record| record.reasoning_effort)
            .as_deref(),
        Some("minimal")
    );
}

#[test]
fn effort_entry_variant_reaches_resolved_record() {
    let agents = roster(vec![AgentDefinition {
        models: candidate_entry(AgentModelCandidate {
            variant: text("low"),
            ..candidate("anthropic/claude-haiku-4-5")
        }),
        ..AgentDefinition::named("explore")
    }]);
    let models = registry(vec![model("anthropic", "claude-haiku-4-5")]);
    let result = expect_resolved(resolve("explore", &agents, &models));
    assert_eq!(
        result
            .resolved_model
            .and_then(|record| record.variant)
            .as_deref(),
        Some("low")
    );
}

#[test]
fn effort_top_level_applies_to_primary_model() {
    let agents = roster(vec![AgentDefinition {
        model: text("quotio-openai/gpt-5.6-luna-fast"),
        reasoning_effort: text("minimal"),
        models: Some(vec![AgentModelEntry::Model(
            "quotio-openai/gpt-5.6-luna-fast".to_string(),
        )]),
        ..AgentDefinition::named("librarian")
    }]);
    let models = registry(vec![model("quotio-openai", "gpt-5.6-luna-fast")]);
    let result = expect_resolved(resolve("librarian", &agents, &models));
    assert_eq!(
        result
            .resolved_model
            .and_then(|record| record.reasoning_effort)
            .as_deref(),
        Some("minimal")
    );
}

#[test]
fn effort_fallback_entry_effort_wins_over_agent_default() {
    let agents = roster(vec![AgentDefinition {
        model: text("kimi-coding/kimi-for-coding-highspeed"),
        reasoning_effort: text("high"),
        models: candidate_entry(AgentModelCandidate {
            reasoning_effort: text("minimal"),
            ..candidate("quotio-openai/gpt-5.6-luna-fast")
        }),
        ..AgentDefinition::named("explore")
    }]);
    let models = registry(vec![model("quotio-openai", "gpt-5.6-luna-fast")]);
    let result = expect_resolved(resolve("explore", &agents, &models));
    assert_eq!(result.model, "quotio-openai/gpt-5.6-luna-fast");
    assert_eq!(
        result
            .resolved_model
            .and_then(|record| record.reasoning_effort)
            .as_deref(),
        Some("minimal")
    );
}

#[test]
fn effort_entry_without_effort_invents_none() {
    let agents = roster(vec![AgentDefinition {
        models: candidate_entry(candidate("openai/plain")),
        ..AgentDefinition::named("explore")
    }]);
    let models = registry(vec![model("openai", "plain")]);
    let record = expect_resolved(resolve("explore", &agents, &models))
        .resolved_model
        .expect("record");
    assert_eq!(record.reasoning_effort, None);
    assert_eq!(record.variant, None);
}

// ---- agents/agent-builtin-chain-tuning.test.ts ----

#[test]
fn chain_tuning_top_level_effort_reaches_record() {
    let agents = roster(vec![AgentDefinition {
        reasoning_effort: text("minimal"),
        ..AgentDefinition::named("explore")
    }]);
    let models = registry(vec![model("openai", "gpt-5.6-luna-fast")]);
    let result = expect_resolved(resolve("explore", &agents, &models));
    assert_eq!(
        result
            .resolved_model
            .and_then(|record| record.reasoning_effort)
            .as_deref(),
        Some("minimal")
    );
}

#[test]
fn chain_tuning_configured_variant_wins_over_rung() {
    let agents = roster(vec![AgentDefinition {
        variant: text("low"),
        ..AgentDefinition::named("momus")
    }]);
    let models = registry(vec![model("openai", "gpt-5.6-terra")]);
    let result = expect_resolved(resolve("momus", &agents, &models));
    assert_eq!(
        result
            .resolved_model
            .and_then(|record| record.variant)
            .as_deref(),
        Some("low")
    );
}

#[test]
fn chain_tuning_rung_variant_survives_without_configured_tuning() {
    let agents = roster(vec![AgentDefinition::named("momus")]);
    let models = registry(vec![model("openai", "gpt-5.6-terra")]);
    let record = expect_resolved(resolve("momus", &agents, &models))
        .resolved_model
        .expect("record");
    assert_eq!(record.variant.as_deref(), Some("high"));
    assert_eq!(record.reasoning_effort, None);
}

// ---- agents/resolve-agent.test.ts ----

fn explore_with_prompt() -> AgentDefinition {
    AgentDefinition {
        prompt: text("Inspect the codebase"),
        ..AgentDefinition::named("explore")
    }
}

#[test]
fn resolve_denylist_rides_persona() {
    let agents = roster(vec![AgentDefinition {
        disallowed_tools: Some(vec!["bash".to_string(), "write".to_string()]),
        ..explore_with_prompt()
    }]);
    let models = registry(vec![model("openai", "gpt-5.6-luna-fast")]);
    let result = expect_resolved(resolve("explore", &agents, &models));
    assert_eq!(
        result.persona.tool_denylist,
        Some(vec!["bash".to_string(), "write".to_string()])
    );
}

#[test]
fn resolve_without_disallowed_tools_has_no_denylist() {
    let agents = roster(vec![explore_with_prompt()]);
    let models = registry(vec![model("openai", "gpt-5.6-luna-fast")]);
    assert_eq!(
        expect_resolved(resolve("explore", &agents, &models))
            .persona
            .tool_denylist,
        None
    );
}

#[test]
fn resolve_fallback_chain_returns_metadata_and_persona() {
    let agents = roster(vec![AgentDefinition {
        execution_mode: text("in-process"),
        ..explore_with_prompt()
    }]);
    let models = registry(vec![model("openai", "gpt-5.6-luna-fast")]);
    let result = expect_resolved(resolve("explore", &agents, &models));
    assert_eq!(result.model, "openai/gpt-5.6-luna-fast");
    let mut expected =
        ResolvedModelRecord::new(ResolvedModelSource::Agent, "openai", "gpt-5.6-luna-fast");
    expected.variant = text("low");
    expected.reasoning = text("low");
    assert_eq!(result.resolved_model, Some(expected));
    assert_eq!(result.persona.agent_type, "explore");
    assert_eq!(
        result.persona.instructions.as_deref(),
        Some("Inspect the codebase")
    );
    assert_eq!(
        result.persona.agent_execution_mode,
        Some(AgentExecutionMode::InProcess)
    );
}

#[test]
fn resolve_def_model_wins_over_def_models() {
    let agents = roster(vec![AgentDefinition {
        model: text("local/primary"),
        models: Some(vec![AgentModelEntry::Model("openai/secondary".to_string())]),
        ..AgentDefinition::named("custom")
    }]);
    let models = registry(vec![
        model("local", "primary"),
        model("openai", "secondary"),
    ]);
    assert_eq!(
        expect_resolved(resolve("custom", &agents, &models)).model,
        "local/primary"
    );
}

#[test]
fn resolve_retains_ordered_runtime_chain() {
    let agents = roster(vec![AgentDefinition {
        model: text("local/primary"),
        models: Some(vec![
            AgentModelEntry::Model("openai/secondary".to_string()),
            AgentModelEntry::Model("google/tertiary".to_string()),
        ]),
        ..AgentDefinition::named("custom")
    }]);
    let models = registry(vec![
        model("openai", "secondary"),
        model("google", "tertiary"),
    ]);
    let result = expect_resolved(resolve("custom", &agents, &models));
    assert_eq!(result.model, "openai/secondary");
    let requested = result.requested_model.expect("requested");
    assert_eq!(
        (
            requested.source,
            requested.provider.as_str(),
            requested.model_id.as_str(),
            requested.display.as_str()
        ),
        (
            ResolvedModelSource::Agent,
            "local",
            "primary",
            "local/primary"
        )
    );
    let fallbacks: Vec<(ResolvedModelSource, String)> = result
        .fallback_models
        .expect("fallbacks")
        .into_iter()
        .map(|record| (record.source, record.display))
        .collect();
    assert_eq!(
        fallbacks,
        vec![(ResolvedModelSource::Agent, "google/tertiary".to_string())]
    );
}

#[test]
fn resolve_first_available_def_models_entry_wins() {
    let agents = roster(vec![AgentDefinition {
        model: text("local/missing"),
        models: Some(vec![
            AgentModelEntry::Model("openai/first".to_string()),
            AgentModelEntry::Model("openai/second".to_string()),
        ]),
        ..AgentDefinition::named("custom")
    }]);
    let models = registry(vec![model("openai", "first"), model("openai", "second")]);
    assert_eq!(
        expect_resolved(resolve("custom", &agents, &models)).model,
        "openai/first"
    );
}

#[test]
fn resolve_keyless_entries_fall_through() {
    let agents = roster(vec![AgentDefinition {
        model: text("keyless/primary"),
        models: Some(vec![
            AgentModelEntry::Model("keyless/secondary".to_string()),
            AgentModelEntry::Model("openai/available".to_string()),
        ]),
        ..AgentDefinition::named("custom")
    }]);
    let models = catalog_registry(
        vec![model("openai", "available")],
        vec![
            model("keyless", "primary"),
            model("keyless", "secondary"),
            model("openai", "available"),
        ],
    );
    assert_eq!(
        expect_resolved(resolve("custom", &agents, &models)).model,
        "openai/available"
    );
}

#[test]
fn resolve_all_keyless_uses_builtin_chain() {
    let agents = roster(vec![AgentDefinition {
        models: Some(vec![AgentModelEntry::Model(
            "anthropic/claude-haiku-4-5".to_string(),
        )]),
        ..AgentDefinition::named("explore")
    }]);
    let models = catalog_registry(
        vec![model("openai", "gpt-5.6-luna-fast")],
        vec![
            model("anthropic", "claude-haiku-4-5"),
            model("openai", "gpt-5.6-luna-fast"),
        ],
    );
    assert_eq!(
        expect_resolved(resolve("explore", &agents, &models)).model,
        "openai/gpt-5.6-luna-fast"
    );
}

#[test]
fn resolve_disabled_agent_is_not_found() {
    let agents = roster(vec![
        AgentDefinition {
            disable: Some(true),
            ..AgentDefinition::named("explore")
        },
        AgentDefinition {
            model: text("openai/momus"),
            ..AgentDefinition::named("momus")
        },
    ]);
    assert_eq!(
        resolve("explore", &agents, &registry(vec![])),
        AgentResolutionResult::NotFound {
            agent: "explore".to_string(),
            available_agents: vec!["momus".to_string()],
        }
    );
}

#[test]
fn resolve_unknown_agent_returns_sorted_roster() {
    let agents = roster(vec![
        AgentDefinition {
            model: text("openai/momus"),
            ..AgentDefinition::named("momus")
        },
        AgentDefinition {
            model: text("openai/explore"),
            ..AgentDefinition::named("explore")
        },
    ]);
    assert_eq!(
        resolve("missing", &agents, &registry(vec![])),
        AgentResolutionResult::NotFound {
            agent: "missing".to_string(),
            available_agents: vec!["explore".to_string(), "momus".to_string()],
        }
    );
}

#[test]
fn resolve_no_match_is_model_unavailable() {
    let agents = roster(vec![AgentDefinition {
        model: text("local/missing"),
        ..AgentDefinition::named("custom")
    }]);
    assert_eq!(
        resolve("custom", &agents, &registry(vec![])),
        AgentResolutionResult::ModelUnavailable {
            agent: "custom".to_string(),
            attempted_model: text("local/missing"),
            available_agents: vec!["custom".to_string()],
        }
    );
}

#[test]
fn resolve_override_without_registry_filters_allowlist() {
    let agents = roster(vec![AgentDefinition {
        prompt: text("Advise only"),
        execution_mode: text("in-process"),
        allowed_subagents: Some(vec!["explore".to_string()]),
        max_depth: Some(2),
        tools: Some(vec![
            AgentToolRule::new("read", true),
            AgentToolRule::new("grep", false),
            AgentToolRule::new("lsp_*", true),
            AgentToolRule::new("bash git status", true),
            AgentToolRule::new("lsp_diagnostics", true),
        ]),
        ..AgentDefinition::named("momus")
    }]);
    let result = expect_resolved(resolve_agent(
        "momus",
        &agents,
        None,
        &ResolveAgentOptions {
            model_override: text("openai/explicit"),
        },
    ));
    assert_eq!(result.model, "openai/explicit");
    assert_eq!(result.resolved_model, None);
    assert_eq!(result.persona.instructions.as_deref(), Some("Advise only"));
    assert_eq!(
        result.persona.tool_allowlist,
        Some(vec!["read".to_string(), "lsp_diagnostics".to_string()])
    );
    assert_eq!(
        result.persona.agent_execution_mode,
        Some(AgentExecutionMode::InProcess)
    );
    assert_eq!(
        result.persona.allowed_subagents,
        Some(vec!["explore".to_string()])
    );
    assert_eq!(result.persona.max_depth, Some(2));
}

// ---- agents/loader.test.ts ----

struct Fixture {
    _root: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::Builder::new()
        .prefix("senpi-agents-")
        .tempdir()
        .expect("tempdir");
    let home = root.path().join("home");
    let project = root.path().join("project");
    Fixture {
        _root: root,
        home,
        project,
    }
}

fn write_text(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, content).expect("write");
}

fn agent_markdown(model: &str, prompt: &str) -> String {
    format!("---\ndescription: {model} agent\nmodel: {model}\n---\n{prompt}\n")
}

fn load(
    fixture: &Fixture,
    registry: &AgentRegistry,
    env: Option<Vec<(&str, &str)>>,
) -> LoadAgentsResult {
    load_agents_with_registry(
        &LoadAgentsOptions {
            env: env.map(|pairs| {
                pairs
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect()
            }),
            home_dir: Some(fixture.home.to_string_lossy().into_owned()),
            project_dir: Some(fixture.project.to_string_lossy().into_owned()),
        },
        registry,
    )
}

fn shown(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_diagnostic_at<'a>(result: &'a LoadAgentsResult, path: &Path) -> &'a AgentLoaderDiagnostic {
    result
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.kind == AgentLoaderDiagnosticKind::Read && diagnostic.path == shown(path)
        })
        .unwrap_or_else(|| panic!("no read diagnostic at {path:?}: {:?}", result.diagnostics))
}

#[test]
fn loader_later_locations_override_earlier_names() {
    let fixture = fixture();
    write_text(
        &fixture.home.join(".pi/agent/agent/finder.md"),
        &agent_markdown("home-pi", "home pi"),
    );
    write_text(
        &fixture.home.join(".senpi/agent/agent/finder.md"),
        &agent_markdown("home-senpi", "home senpi"),
    );
    write_text(
        &fixture.project.join(".pi/agent/finder.md"),
        &agent_markdown("project-pi", "project pi"),
    );
    write_text(
        &fixture.project.join(".senpi/agents/agents/finder.md"),
        &agent_markdown("project-senpi", "project senpi"),
    );
    let result = load(&fixture, &AgentRegistry::new(), None);
    assert_eq!(result.diagnostics, vec![]);
    let finder = result.agent("finder").expect("finder");
    assert_eq!(finder.model.as_deref(), Some("project-senpi"));
    assert_eq!(finder.prompt.as_deref(), Some("project senpi\n"));
}

#[test]
fn loader_scans_only_agent_subdirectories() {
    let fixture = fixture();
    write_text(
        &fixture.project.join(".senpi/ignored.md"),
        &agent_markdown("ignored", "ignored"),
    );
    write_text(
        &fixture.project.join(".senpi/agent/kept.md"),
        &agent_markdown("kept", "kept"),
    );
    let result = load(&fixture, &AgentRegistry::new(), None);
    assert!(result.agent("ignored").is_none());
    assert_eq!(
        result
            .agent("kept")
            .and_then(|agent| agent.model.as_deref()),
        Some("kept")
    );
}

#[test]
fn loader_registered_agent_overrides_file() {
    let fixture = fixture();
    write_text(
        &fixture.project.join(".pi/agents/writer.md"),
        &agent_markdown("file-model", "file prompt"),
    );
    let registry = AgentRegistry::new();
    registry.register(AgentDefinition {
        model: text("registered-model"),
        prompt: text("registered prompt"),
        ..AgentDefinition::named("writer")
    });
    let result = load(&fixture, &registry, None);
    let writer = result.agent("writer").expect("writer");
    assert_eq!(writer.model.as_deref(), Some("registered-model"));
    assert_eq!(writer.prompt.as_deref(), Some("registered prompt"));
}

#[test]
fn loader_omo_overlay_wins_last() {
    let fixture = fixture();
    write_text(
        &fixture.project.join(".pi/agents/finder.md"),
        &agent_markdown("file-model", "file prompt"),
    );
    let registry = AgentRegistry::new();
    registry.register(AgentDefinition {
        model: text("registered-model"),
        ..AgentDefinition::named("finder")
    });
    write_text(
        &fixture.project.join(".omo/omo.json"),
        r#"{"agents":{"finder":{"model":"omo-model"}}}"#,
    );
    let result = load(&fixture, &registry, None);
    let finder = result.agent("finder").expect("finder");
    assert_eq!(finder.model.as_deref(), Some("omo-model"));
    assert_eq!(finder.prompt.as_deref(), Some("file prompt\n"));
}

#[test]
fn loader_malformed_frontmatter_is_per_file() {
    let fixture = fixture();
    let bad_path = fixture.project.join(".senpi/agent/broken.md");
    write_text(&bad_path, "---\nmodel: [unterminated\n---\nBad");
    write_text(
        &fixture.project.join(".senpi/agent/valid.md"),
        &agent_markdown("valid-model", "Valid"),
    );
    let result = load(&fixture, &AgentRegistry::new(), None);
    assert_eq!(
        result
            .agent("valid")
            .and_then(|agent| agent.model.as_deref()),
        Some("valid-model")
    );
    assert!(result.agent("broken").is_none());
    assert!(result.diagnostics.contains(&AgentLoaderDiagnostic {
        kind: AgentLoaderDiagnosticKind::Frontmatter,
        path: shown(&bad_path),
        message: format!("Malformed YAML frontmatter in {}", shown(&bad_path)),
        issue_paths: None,
    }));
}

#[cfg(unix)]
#[test]
fn loader_symlinked_scan_root_is_blocked() {
    let fixture = fixture();
    let external_root = fixture.home.join("external-agents");
    let linked_root = fixture.project.join(".senpi/agent");
    write_text(
        &external_root.join("agent/linked.md"),
        &agent_markdown("external-model", "external prompt"),
    );
    fs::create_dir_all(fixture.project.join(".senpi")).expect("mkdir");
    std::os::unix::fs::symlink(&external_root, &linked_root).expect("symlink");
    let result = load(&fixture, &AgentRegistry::new(), None);
    assert!(result.agent("linked").is_none());
    let diagnostic = read_diagnostic_at(&result, &linked_root);
    assert!(diagnostic.message.contains(&shown(&linked_root)));
    assert!(diagnostic.message.contains("symlink"));
}

#[cfg(unix)]
#[test]
fn loader_symlinked_location_does_not_escape() {
    let fixture = fixture();
    let external_root = fixture.home.join("external-agents");
    let linked_root = fixture.project.join(".senpi/agents");
    write_text(
        &external_root.join("agent/escaped.md"),
        &agent_markdown("external-model", "external prompt"),
    );
    fs::create_dir_all(fixture.project.join(".senpi")).expect("mkdir");
    std::os::unix::fs::symlink(&external_root, &linked_root).expect("symlink");
    let result = load(&fixture, &AgentRegistry::new(), None);
    assert!(result.agent("escaped").is_none());
    assert!(
        read_diagnostic_at(&result, &linked_root)
            .message
            .contains("symlink")
    );
}

#[cfg(unix)]
#[test]
fn loader_broken_symlink_reports_and_sibling_loads() {
    let fixture = fixture();
    let broken_link = fixture.project.join(".senpi/agent/nested/missing");
    write_text(
        &fixture.project.join(".senpi/agent/valid.md"),
        &agent_markdown("valid-model", "Valid"),
    );
    fs::create_dir_all(broken_link.parent().expect("parent")).expect("mkdir");
    std::os::unix::fs::symlink(fixture.project.join("does-not-exist"), &broken_link)
        .expect("symlink");
    let result = load(&fixture, &AgentRegistry::new(), None);
    assert_eq!(
        result
            .agent("valid")
            .and_then(|agent| agent.model.as_deref()),
        Some("valid-model")
    );
    assert!(
        read_diagnostic_at(&result, &broken_link)
            .message
            .contains(&shown(&broken_link))
    );
}

#[test]
fn loader_omo_json_directory_reports_read_diagnostic() {
    let fixture = fixture();
    let config_path = fixture.project.join(".omo/omo.json");
    write_text(
        &fixture.project.join(".senpi/agent/valid.md"),
        &agent_markdown("valid-model", "Valid"),
    );
    fs::create_dir_all(&config_path).expect("mkdir");
    let result = load(&fixture, &AgentRegistry::new(), None);
    assert_eq!(
        result
            .agent("valid")
            .and_then(|agent| agent.model.as_deref()),
        Some("valid-model")
    );
    assert!(
        read_diagnostic_at(&result, &config_path)
            .message
            .contains(&shown(&config_path))
    );
}

#[test]
fn loader_repeated_tool_rules_last_match_wins() {
    let fixture = fixture();
    write_text(
        &fixture.project.join(".pi/agent/guarded.md"),
        "---\ntools:\n  - pattern: shell\n    allow: true\n  - pattern: read\n    action: allow\n  - pattern: shell\n    deny: true\n---\nGuarded\n",
    );
    let result = load(&fixture, &AgentRegistry::new(), None);
    let rules = result
        .agent("guarded")
        .and_then(|agent| agent.tools.clone())
        .unwrap_or_default();
    assert_eq!(resolve_tool_rule(&rules, "read"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "shell"), Some(false));
    assert_eq!(resolve_tool_rule(&rules, "write"), None);
}

#[test]
fn loader_pi_task_tools_frontmatter_shape_evaluates() {
    let fixture = fixture();
    let agent_path = fixture.project.join(".senpi/agent/finder.md");
    write_text(
        &agent_path,
        "---\ndescription: Find facts with read-only tools\ntools:\n  read: allow\n  write: deny\n  task:\n    \"web-librarian\": allow\n  bash:\n    \"*\": deny\n    \"rg *\": allow\n---\nYou are a careful finder.\n",
    );
    let result = load(&fixture, &AgentRegistry::new(), None);
    let finder = result.agent("finder").expect("finder");
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.path != shown(&agent_path))
    );
    let rules = finder.tools.clone().unwrap_or_default();
    assert_eq!(resolve_tool_rule(&rules, "read"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "write"), Some(false));
    assert_eq!(resolve_tool_rule(&rules, "task web-librarian"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "bash rg foo"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "bash ls"), Some(false));
}

#[test]
fn loader_string_action_tools_record_is_not_dropped() {
    let fixture = fixture();
    write_text(
        &fixture.project.join(".pi/agent/reader.md"),
        "---\ntools:\n  read: allow\n  shell: deny\n---\nReader\n",
    );
    let result = load(&fixture, &AgentRegistry::new(), None);
    let reader = result.agent("reader").expect("reader");
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == AgentLoaderDiagnosticKind::Validation)
    );
    let rules = reader.tools.clone().unwrap_or_default();
    assert_eq!(resolve_tool_rule(&rules, "read"), Some(true));
    assert_eq!(resolve_tool_rule(&rules, "shell"), Some(false));
}

#[test]
fn loader_senpi_profile_view_and_model_catalog_win() {
    let fixture = fixture();
    write_text(
        &fixture.project.join(".omo/omo.json"),
        &json!({
            "agents": { "finder": { "model": "base" } },
            "models": { "quick": { "model": "provider/quick", "reasoningEffort": "low" } },
            "[senpi]": { "agents": { "finder": { "model": "quick" } } },
            "profiles": { "focused": { "[senpi]": { "agents": { "finder": { "model": "provider/focused" } } } } },
        })
        .to_string(),
    );
    let result = load(
        &fixture,
        &AgentRegistry::new(),
        Some(vec![("OMO_PROFILE", "focused")]),
    );
    assert_eq!(
        result
            .agent("finder")
            .and_then(|agent| agent.model.as_deref()),
        Some("provider/focused")
    );
}

#[test]
fn loader_snake_case_omo_keys_normalize() {
    let fixture = fixture();
    write_text(
        &fixture.project.join(".omo/omo.json"),
        &json!({ "agents": { "planner": {
            "execution_mode": "process",
            "allowed_subagents": ["worker"],
            "disallowed_tools": ["shell"],
            "max_depth": 2,
            "max_turns": 9,
        } } })
        .to_string(),
    );
    let result = load(&fixture, &AgentRegistry::new(), None);
    let planner = result.agent("planner").expect("planner");
    assert_eq!(planner.execution_mode.as_deref(), Some("process"));
    assert_eq!(planner.allowed_subagents, Some(vec!["worker".to_string()]));
    assert_eq!(planner.disallowed_tools, Some(vec!["shell".to_string()]));
    assert_eq!(planner.max_depth, Some(2));
    assert_eq!(planner.max_turns, Some(9));
}

#[test]
fn register_agent_feeds_process_wide_loader() {
    let fixture = fixture();
    register_agent(AgentDefinition {
        model: text("process-wide"),
        ..AgentDefinition::named("senpi-task-process-wide-probe")
    });
    let result = load_agents(&LoadAgentsOptions {
        env: None,
        home_dir: Some(shown(&fixture.home)),
        project_dir: Some(shown(&fixture.project)),
    });
    assert_eq!(
        result
            .agent("senpi-task-process-wide-probe")
            .and_then(|agent| agent.model.as_deref()),
        Some("process-wide")
    );
}
