//! Agent resolution against the host model registry (`agents/resolve-agent.ts`,
//! `agents/agent-model-registry.ts`).

use std::collections::BTreeSet;

use super::builtin::agent_fallback_chain;
use super::types::{AgentDefinition, AgentModelEntry, agent_model_candidates};
use crate::delegate_adapter::{
    DelegateModelResolutionInput, DelegateModelResolutionResult, resolve_model_for_delegate_task,
    warm_cache_deps,
};
use crate::host::{
    HostError, ParsedModel, SenpiModelRegistry, parse_available_models, parse_model,
    parse_registry_model,
};
use crate::model_chain::{
    BuildModelChainOptions, ChainRungCandidateOptions, RuntimeModelChain,
    build_runtime_model_chain, chain_rung_candidates,
};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveAgentOptions {
    pub model_override: Option<String>,
    /// `omoConfig.exclude_models`: the registry is filtered through it before any candidate is read.
    pub exclude_models: Option<Vec<String>>,
}

/// `provider/model-id` of one registry entry (`model-exclusion.ts` `modelKey`); `None` when the entry
/// is not an object carrying both string fields.
fn registry_model_key(model: &serde_json::Value) -> Option<String> {
    let object = model.as_object()?;
    let provider = object.get("provider")?.as_str()?;
    let id = object.get("id")?.as_str()?;
    Some(format!("{provider}/{id}"))
}

/// `excluded-models.ts` `patternExpression`: `*` matches any run of characters (including `/` and
/// none at all) and every other character is literal; matching ignores case.
fn wildcard_pattern_matches(pattern: &str, subject: &str) -> bool {
    let pattern: Vec<char> = pattern.to_lowercase().chars().collect();
    let subject: Vec<char> = subject.to_lowercase().chars().collect();
    let (mut p, mut s) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while s < subject.len() {
        if p < pattern.len() && pattern[p] != '*' && pattern[p] == subject[s] {
            p += 1;
            s += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            mark = s;
        } else if let Some(star_at) = star {
            p = star_at + 1;
            mark += 1;
            s = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

/// `excluded-models.ts` `selectorTails`: every tail starting after a `/` is a model id some provider
/// serves, so an entry is tested against each of them.
fn selector_tails(model: &str) -> Vec<&str> {
    let tails: Vec<&str> = model
        .match_indices('/')
        .map(|(index, _)| &model[index + 1..])
        .collect();
    if tails.is_empty() {
        vec![model]
    } else {
        tails
    }
}

/// The entry that keeps `model` out, or `None` (`excluded-models.ts` `excludedModelPattern`).
pub(crate) fn excluded_model_pattern<'a>(model: &str, patterns: &'a [String]) -> Option<&'a str> {
    let tails = selector_tails(model);
    for raw in patterns {
        let pattern = raw.trim();
        if pattern.is_empty() {
            continue;
        }
        let matched = if pattern.contains('/') {
            wildcard_pattern_matches(pattern, model)
                || tails
                    .iter()
                    .any(|tail| wildcard_pattern_matches(pattern, tail))
        } else {
            tails
                .iter()
                .any(|tail| wildcard_pattern_matches(pattern, tail))
        };
        if matched {
            return Some(raw.as_str());
        }
    }
    None
}

pub(crate) fn is_excluded_model(model: &str, patterns: &[String]) -> bool {
    excluded_model_pattern(model, patterns).is_some()
}

/// The registry as `exclude_models` leaves it (`model-exclusion.ts` `withoutExcludedModels`): an
/// excluded model is neither available nor found, so every chain walk skips it exactly as it skips a
/// model whose provider has no login.
struct ExcludedModelsRegistry<'a> {
    inner: &'a dyn SenpiModelRegistry,
    patterns: Vec<String>,
}

impl SenpiModelRegistry for ExcludedModelsRegistry<'_> {
    fn get_available(&self) -> Result<serde_json::Value, HostError> {
        let available = self.inner.get_available()?;
        let serde_json::Value::Array(models) = available else {
            return Ok(available);
        };
        Ok(serde_json::Value::Array(
            models
                .into_iter()
                .filter(|model| match registry_model_key(model) {
                    None => true,
                    Some(key) => !is_excluded_model(&key, &self.patterns),
                })
                .collect(),
        ))
    }

    fn find(&self, provider: &str, model_id: &str) -> Option<serde_json::Value> {
        if is_excluded_model(&format!("{provider}/{model_id}"), &self.patterns) {
            return None;
        }
        self.inner.find(provider, model_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentExecutionMode {
    InProcess,
    Process,
}

/// The persona fields a resolved agent carries into its child spec.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentPersona {
    pub agent_type: String,
    pub instructions: Option<String>,
    /// Literal allow rules only (no wildcards, no command-scoped patterns).
    pub tool_allowlist: Option<Vec<String>>,
    /// The definition's `disallowed_tools`, carried through to the child's tool exclusion.
    pub tool_denylist: Option<Vec<String>>,
    pub agent_execution_mode: Option<AgentExecutionMode>,
    pub allowed_subagents: Option<Vec<String>>,
    pub max_depth: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedAgent {
    pub agent: String,
    pub model: String,
    pub requested_model: Option<ResolvedModelRecord>,
    pub fallback_models: Option<Vec<ResolvedModelRecord>>,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub available_agents: Vec<String>,
    pub persona: AgentPersona,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentResolutionResult {
    Resolved(Box<ResolvedAgent>),
    NotFound {
        agent: String,
        available_agents: Vec<String>,
    },
    ModelUnavailable {
        agent: String,
        attempted_model: Option<String>,
        available_agents: Vec<String>,
    },
}

fn find_exact_agent_model(
    candidate: &str,
    registry: &dyn SenpiModelRegistry,
) -> Option<ParsedModel> {
    let expected = parse_model(candidate)?;
    let found = registry.find(&expected.provider, &expected.model_id)?;
    let parsed = parse_registry_model(&found, Some(&expected))?;
    Some(ParsedModel {
        provider: parsed.provider,
        model_id: parsed.model_id,
    })
}

/// Resolves `name` from `agents`: an override wins outright; otherwise every configured candidate
/// is gated on the auth-filtered available set (so keyless catalog entries fall through), then
/// the builtin fallback chain takes over.
pub fn resolve_agent(
    name: &str,
    agents: &[(String, AgentDefinition)],
    registry: Option<&dyn SenpiModelRegistry>,
    options: &ResolveAgentOptions,
) -> AgentResolutionResult {
    // `exclude_models` filters the registry before any candidate is read, so an excluded model never
    // resolves even when the machine holds credentials for it.
    let excluded = options.exclude_models.clone().unwrap_or_default();
    let filtered;
    let registry = match registry {
        Some(inner) if !excluded.is_empty() => {
            filtered = ExcludedModelsRegistry {
                inner,
                patterns: excluded,
            };
            Some(&filtered as &dyn SenpiModelRegistry)
        }
        other => other,
    };
    let mut available_agents: Vec<String> = agents
        .iter()
        .filter(|(_, definition)| definition.disable != Some(true))
        .map(|(agent_name, _)| agent_name.clone())
        .collect();
    available_agents.sort();
    let definition = agents
        .iter()
        .find(|(agent_name, _)| agent_name == name)
        .map(|(_, definition)| definition)
        .filter(|definition| definition.disable != Some(true));
    let Some(definition) = definition else {
        return AgentResolutionResult::NotFound {
            agent: name.to_string(),
            available_agents,
        };
    };

    let persona = agent_persona(name, definition);
    if let Some(model) = &options.model_override {
        return AgentResolutionResult::Resolved(Box::new(ResolvedAgent {
            agent: name.to_string(),
            model: model.clone(),
            requested_model: None,
            fallback_models: None,
            resolved_model: None,
            available_agents,
            persona,
        }));
    }

    let fallback_chain = agent_fallback_chain(name);
    let Some(registry) = registry else {
        let attempted_model = definition
            .model
            .clone()
            .or_else(|| {
                definition
                    .models
                    .as_ref()
                    .and_then(|models| models.first())
                    .map(|entry| entry.model().to_string())
            })
            .or_else(|| {
                let head = fallback_chain?.first()?;
                let provider = head.providers.first()?;
                Some(format!("{provider}/{}", head.model))
            });
        return AgentResolutionResult::ModelUnavailable {
            agent: name.to_string(),
            attempted_model,
            available_agents,
        };
    };

    let available_models: Option<BTreeSet<String>> = registry
        .get_available()
        .ok()
        .and_then(|models| parse_available_models(&models))
        .map(|models| models.into_iter().collect());
    let resolve = |found: ParsedModel,
                   variant: Option<String>,
                   reasoning_effort: Option<String>,
                   chain: RuntimeModelChain,
                   available_agents: Vec<String>| {
        let mut record =
            ResolvedModelRecord::new(ResolvedModelSource::Agent, &found.provider, &found.model_id);
        record.reasoning = reasoning_effort.clone().or_else(|| variant.clone());
        record.variant = variant;
        record.reasoning_effort = reasoning_effort;
        AgentResolutionResult::Resolved(Box::new(ResolvedAgent {
            agent: name.to_string(),
            model: record.display.clone(),
            requested_model: chain.requested_model,
            fallback_models: chain.fallback_models,
            resolved_model: Some(record),
            available_agents,
            persona: persona.clone(),
        }))
    };

    let mut attempted_model = None;
    let entries: Option<&[AgentModelEntry]> = definition.models.as_deref();
    let direct_models = agent_model_candidates(
        definition.model.as_deref(),
        entries,
        definition.variant.as_deref(),
        definition.reasoning_effort.as_deref(),
    );
    for candidate in &direct_models {
        attempted_model = Some(candidate.model.clone());
        let Some(found) = find_exact_agent_model(&candidate.model, registry) else {
            continue;
        };
        if let Some(available) = &available_models
            && !available.contains(&format!("{}/{}", found.provider, found.model_id))
        {
            continue;
        }
        let chain = build_runtime_model_chain(&BuildModelChainOptions {
            candidates: direct_models.clone(),
            selected_model: &candidate.model,
            available_models: available_models.as_ref(),
            source: ResolvedModelSource::Agent,
        });
        return resolve(
            found,
            candidate.variant.clone(),
            candidate.reasoning_effort.clone(),
            chain,
            available_agents,
        );
    }

    if let (Some(available), Some(chain)) = (&available_models, fallback_chain)
        && let Some(DelegateModelResolutionResult::Resolved(resolution)) = resolve_model_for_delegate_task(
            &DelegateModelResolutionInput {
                fallback_chain: Some(chain.to_vec()),
                available_models: available.iter().cloned().collect(),
                ..Default::default()
            },
            &warm_cache_deps(),
        )
    {
        attempted_model = Some(resolution.model.clone());
        if let Some(found) = find_exact_agent_model(&resolution.model, registry) {
            let runtime_chain = build_runtime_model_chain(&BuildModelChainOptions {
                candidates: chain_rung_candidates(&ChainRungCandidateOptions {
                    chain,
                    selected_model: &resolution.model,
                    selected_rung_entry: resolution.fallback_entry.as_ref(),
                    available_models: available,
                }),
                selected_model: &resolution.model,
                available_models: Some(available),
                source: ResolvedModelSource::Agent,
            });
            // Configured tuning wins over the rung's own variant.
            return resolve(
                found,
                definition.variant.clone().or(resolution.variant),
                definition.reasoning_effort.clone(),
                runtime_chain,
                available_agents,
            );
        }
    }

    AgentResolutionResult::ModelUnavailable {
        agent: name.to_string(),
        attempted_model,
        available_agents,
    }
}

fn agent_persona(name: &str, definition: &AgentDefinition) -> AgentPersona {
    AgentPersona {
        agent_type: name.to_string(),
        instructions: definition.prompt.clone(),
        tool_allowlist: definition.tools.as_ref().map(|rules| {
            rules
                .iter()
                .filter(|rule| {
                    rule.allow && !rule.pattern.contains(' ') && !rule.pattern.contains('*')
                })
                .map(|rule| rule.pattern.clone())
                .collect()
        }),
        tool_denylist: definition.disallowed_tools.clone(),
        agent_execution_mode: match definition.execution_mode.as_deref() {
            Some("in-process") => Some(AgentExecutionMode::InProcess),
            Some("process") => Some(AgentExecutionMode::Process),
            Some(_) | None => None,
        },
        allowed_subagents: definition.allowed_subagents.clone(),
        max_depth: definition.max_depth,
    }
}
