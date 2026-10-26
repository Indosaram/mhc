use maho_ai::cursor::model_capabilities::get_cursor_variant_alias;
use maho_ai::types::{Model, ModelThinkingLevel, ThinkingSelection, ThinkingSelectionSource};

#[derive(Debug, Clone, Default)]
pub struct ParsedModelResult {
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub service_tier: Option<String>,
    pub warning: Option<String>,
}

pub fn get_model_narrowing_patterns<'a>(
    cli: Option<&'a [String]>,
    legacy: Option<&'a [String]>,
) -> &'a [String] {
    cli.or(legacy).unwrap_or(&[])
}

pub fn find_exact_model_reference_match<'a>(
    reference: &str,
    models: &'a [Model],
) -> Option<&'a Model> {
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    let canonical: Vec<_> = models
        .iter()
        .filter(|m| format!("{}/{}", m.provider, m.id).eq_ignore_ascii_case(reference))
        .collect();
    match canonical.as_slice() {
        [model] => return Some(model),
        [] => {}
        _ => return None,
    }
    if let Some((provider, id)) = reference.split_once('/') {
        let matches: Vec<_> = models
            .iter()
            .filter(|m| {
                m.provider.eq_ignore_ascii_case(provider.trim())
                    && m.id.eq_ignore_ascii_case(id.trim())
            })
            .collect();
        match matches.as_slice() {
            [model] => return Some(model),
            [] => {}
            _ => return None,
        }
    }
    let matches: Vec<_> = models
        .iter()
        .filter(|m| m.id.eq_ignore_ascii_case(reference))
        .collect();
    match matches.as_slice() {
        [model] => Some(model),
        _ => None,
    }
}

fn legacy_resolution(reference: &str, models: &[Model]) -> Option<ParsedModelResult> {
    let reference = reference.trim();
    let (provider, variant) = reference
        .split_once('/')
        .map_or((None, reference), |(p, id)| (Some(p), id));
    let eligible = |model: &&Model| {
        matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth")
            && provider.is_none_or(|p| model.provider.eq_ignore_ascii_case(p))
    };
    if let Some(alias) = get_cursor_variant_alias(variant) {
        let candidates: Vec<_> = models
            .iter()
            .filter(eligible)
            .filter(|m| m.id == alias.target_id)
            .collect();
        let [model] = candidates.as_slice() else {
            return None;
        };
        return Some(ParsedModelResult {
            model: Some((*model).clone()),
            thinking_level: alias.level,
            thinking_selection: alias.level.map(|level| ThinkingSelection {
                level,
                source: ThinkingSelectionSource::LegacyVariant,
                legacy_variant_id: Some(variant.into()),
            }),
            ..Default::default()
        });
    }
    let candidates: Vec<_> = models
        .iter()
        .filter(|m| provider.is_none() || eligible(m))
        .cloned()
        .collect();
    if find_exact_model_reference_match(reference, &candidates).is_some() {
        return None;
    }
    let mut derived = Vec::new();
    for model in models.iter().filter(eligible) {
        if let Some(variants) = model
            .compat
            .as_ref()
            .and_then(|c| c.0.get("cursorReasoning"))
            .and_then(|v| v.get("variantIds"))
            .and_then(serde_json::Value::as_object)
        {
            for (level, id) in variants {
                if let (Some(level), Some(id)) = (ModelThinkingLevel::parse(level), id.as_str())
                    && id.eq_ignore_ascii_case(variant)
                {
                    derived.push(ParsedModelResult {
                        model: Some(model.clone()),
                        thinking_level: Some(level),
                        thinking_selection: Some(ThinkingSelection {
                            level,
                            source: ThinkingSelectionSource::LegacyVariant,
                            legacy_variant_id: Some(id.into()),
                        }),
                        ..Default::default()
                    });
                }
            }
        }
    }
    if derived.len() == 1 {
        derived.pop()
    } else {
        None
    }
}

fn is_alias(id: &str) -> bool {
    id.ends_with("-latest")
        || !id.rsplit_once('-').is_some_and(|(_, suffix)| {
            suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_digit())
        })
}

const CURSOR_ALIAS_LEVEL_TOKENS: &[&str] = &[
    "minimal",
    "low",
    "medium",
    "high",
    "extra-high",
    "xhigh",
    "max",
    "none",
];

/// senpi `legacySelection`.
fn legacy_selection(legacy_variant_id: &str) -> Option<ThinkingSelection> {
    let level = get_cursor_variant_alias(legacy_variant_id)?.level?;
    Some(ThinkingSelection {
        level,
        source: ThinkingSelectionSource::LegacyVariant,
        legacy_variant_id: Some(legacy_variant_id.into()),
    })
}

/// senpi `derivedCursorVariantIds`: variant ids a runtime-derived Cursor group observed.
fn derived_cursor_variant_ids(model: &Model) -> Vec<(ModelThinkingLevel, String)> {
    if !((model.provider == "cursor" && model.api == "cursor-agent")
        || (model.provider == "cursor-cli-oauth" && model.api == "cursor-cli-oauth"))
    {
        return Vec::new();
    }
    model
        .compat
        .as_ref()
        .and_then(|c| c.0.get("cursorReasoning"))
        .and_then(|v| v.get("variantIds"))
        .and_then(serde_json::Value::as_object)
        .map(|variants| {
            variants
                .iter()
                .filter_map(|(level, id)| {
                    Some((ModelThinkingLevel::parse(level)?, id.as_str()?.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// senpi `cursorLegacySelection`.
fn cursor_legacy_selection(model: &Model, variant_id: &str) -> Option<ThinkingSelection> {
    if let Some(selection) = legacy_selection(variant_id) {
        return Some(selection);
    }
    let normalized = variant_id.to_lowercase();
    derived_cursor_variant_ids(model)
        .into_iter()
        .find(|(_, id)| id.to_lowercase() == normalized)
        .map(|(level, id)| ThinkingSelection {
            level,
            source: ThinkingSelectionSource::LegacyVariant,
            legacy_variant_id: Some(id),
        })
}

/// senpi `cursorLegacyAliasesForModel`: legacy variant ids that resolve to this Cursor model.
fn cursor_legacy_aliases_for_model(model: &Model) -> Vec<String> {
    if !matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth") {
        return Vec::new();
    }
    let target_id = model.id.as_str();
    let base_id = target_id.strip_suffix("-thinking").unwrap_or(target_id);
    let mut candidates = indexmap::IndexSet::new();
    candidates.insert(target_id.to_owned());
    candidates.insert(format!("{base_id}-thinking"));
    candidates.insert(format!("{base_id}-fast"));
    for level in CURSOR_ALIAS_LEVEL_TOKENS {
        candidates.insert(format!("{base_id}-{level}"));
        candidates.insert(format!("{base_id}-thinking-{level}"));
        candidates.insert(format!("{base_id}-{level}-thinking"));
        candidates.insert(format!("{target_id}-{level}"));
    }
    let mut aliases: Vec<String> = candidates
        .into_iter()
        .filter(|candidate| {
            get_cursor_variant_alias(candidate).is_some_and(|alias| alias.target_id == target_id)
        })
        .collect();
    aliases.extend(
        derived_cursor_variant_ids(model)
            .into_iter()
            .map(|(_, id)| id),
    );
    aliases
}

/// minimatch(`value`, `pattern`, { nocase: true }).
fn minimatch_nocase(value: &str, pattern: &str) -> bool {
    crate::package_manager::minimatch_path(&value.to_lowercase(), &pattern.to_lowercase())
}

fn try_match_model<'a>(pattern: &str, models: &'a [Model]) -> Option<&'a Model> {
    if let Some(model) = find_exact_model_reference_match(pattern, models) {
        return Some(model);
    }
    let pattern = pattern.to_lowercase();
    let mut matches: Vec<_> = models
        .iter()
        .filter(|m| {
            m.id.to_lowercase().contains(&pattern) || m.name.to_lowercase().contains(&pattern)
        })
        .collect();
    matches.sort_by(|a, b| {
        is_alias(&b.id)
            .cmp(&is_alias(&a.id))
            .then_with(|| b.id.cmp(&a.id))
    });
    matches.first().copied()
}

pub fn parse_model_pattern(
    pattern: &str,
    models: &[Model],
    allow_invalid_thinking_level_fallback: bool,
) -> ParsedModelResult {
    if let Some(result) = legacy_resolution(pattern, models) {
        return result;
    }
    if let Some(model) = try_match_model(pattern, models) {
        return ParsedModelResult {
            model: Some(model.clone()),
            ..Default::default()
        };
    }
    let Some((prefix, suffix)) = pattern.rsplit_once(':') else {
        return ParsedModelResult::default();
    };
    let thinking = ModelThinkingLevel::parse(suffix);
    let tier = matches!(suffix, "auto" | "flex" | "priority" | "ultrafast");
    if thinking.is_none() && !tier && !allow_invalid_thinking_level_fallback {
        return ParsedModelResult::default();
    }
    let mut result = parse_model_pattern(prefix, models, allow_invalid_thinking_level_fallback);
    if result.model.is_none() {
        return result;
    }
    if let Some(level) = thinking {
        result.thinking_level = if result.warning.is_some() {
            None
        } else {
            result.thinking_level.or(Some(level))
        };
        result.thinking_selection = result.thinking_level.map(|level| ThinkingSelection {
            level,
            source: ThinkingSelectionSource::Explicit,
            legacy_variant_id: None,
        });
    } else if tier {
        result.service_tier = if result.warning.is_some() {
            None
        } else {
            result.service_tier.or_else(|| Some(suffix.into()))
        };
    } else {
        result.thinking_level = None;
        result.thinking_selection = None;
        result.warning = Some(format!(
            "Invalid thinking level \"{suffix}\" in pattern \"{pattern}\". Using default instead."
        ));
    }
    result
}

pub fn resolve_stored_model_reference(
    provider: &str,
    id: &str,
    models: &[Model],
) -> Option<ParsedModelResult> {
    legacy_resolution(&format!("{provider}/{id}"), models).or_else(|| {
        models
            .iter()
            .find(|m| m.provider == provider && m.id == id)
            .map(|m| ParsedModelResult {
                model: Some(m.clone()),
                ..Default::default()
            })
    })
}

#[derive(Debug, Clone)]
pub struct ScopedModel {
    pub model: Model,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub service_tier: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ModelScopeDiagnostic {
    pub code: &'static str,
    pub message: String,
    pub pattern: String,
}
#[derive(Debug, Clone)]
pub struct PatternResolution {
    pub pattern: String,
    pub owned_ids: Vec<String>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub service_tier: Option<String>,
    pub unresolved: bool,
    pub is_glob: bool,
}
#[derive(Debug, Default)]
pub struct ResolveModelScopeResult {
    pub scoped_models: Vec<ScopedModel>,
    pub diagnostics: Vec<ModelScopeDiagnostic>,
    pub pattern_resolutions: Vec<PatternResolution>,
}

pub fn resolve_model_scope_from_models(
    patterns: &[String],
    models: &[Model],
) -> ResolveModelScopeResult {
    let mut result = ResolveModelScopeResult::default();
    let mut claimed = std::collections::HashSet::new();
    for pattern in patterns {
        let is_glob = pattern.contains(['*', '?', '[']);
        let mut matches = Vec::new();
        let mut thinking = None;
        let mut tier = None;
        if is_glob {
            let mut glob = pattern.as_str();
            while let Some((prefix, suffix)) = glob.rsplit_once(':') {
                if thinking.is_none() && ModelThinkingLevel::parse(suffix).is_some() {
                    thinking = ModelThinkingLevel::parse(suffix);
                } else if tier.is_none()
                    && matches!(suffix, "auto" | "flex" | "priority" | "ultrafast")
                {
                    tier = Some(suffix.to_owned());
                } else {
                    break;
                }
                glob = prefix;
            }
            if let Some(model) = find_exact_model_reference_match(glob, models) {
                matches.push(ParsedModelResult {
                    model: Some(model.clone()),
                    thinking_level: thinking,
                    service_tier: tier.clone(),
                    ..Default::default()
                });
            } else {
                let canonical_pattern = glob.contains('/');
                let glob_matches = |provider: &str, id: &str| {
                    minimatch_nocase(&format!("{provider}/{id}"), glob)
                        || (!canonical_pattern && minimatch_nocase(id, glob))
                };
                // Projection per canonical model, in catalog order: the model itself and/or the
                // Cursor legacy aliases the glob matched.
                let mut projections: indexmap::IndexMap<
                    String,
                    (&Model, Vec<Option<ThinkingSelection>>),
                > = indexmap::IndexMap::new();
                for model in models {
                    let id = format!("{}/{}", model.provider, model.id);
                    if glob_matches(&model.provider, &model.id) {
                        projections.insert(id.clone(), (model, Vec::new()));
                    }
                    for alias_id in cursor_legacy_aliases_for_model(model) {
                        if !glob_matches(&model.provider, &alias_id) {
                            continue;
                        }
                        projections
                            .entry(id.clone())
                            .or_insert_with(|| (model, Vec::new()))
                            .1
                            .push(cursor_legacy_selection(model, &alias_id));
                    }
                }
                for (model, aliases) in projections.into_values() {
                    let (thinking_selection, projected_level) = if let Some(level) = thinking {
                        (
                            Some(ThinkingSelection {
                                level,
                                source: ThinkingSelectionSource::Explicit,
                                legacy_variant_id: None,
                            }),
                            Some(level),
                        )
                    } else {
                        let mut selections: Vec<ThinkingSelection> =
                            aliases.into_iter().flatten().collect();
                        let levels: std::collections::HashSet<_> =
                            selections.iter().map(|selection| selection.level).collect();
                        if levels.len() == 1 && !selections.is_empty() {
                            selections.sort_by(|a, b| {
                                a.legacy_variant_id
                                    .as_deref()
                                    .unwrap_or("")
                                    .cmp(b.legacy_variant_id.as_deref().unwrap_or(""))
                            });
                            let selection = selections.swap_remove(0);
                            let level = selection.level;
                            (Some(selection), Some(level))
                        } else {
                            (None, None)
                        }
                    };
                    matches.push(ParsedModelResult {
                        model: Some(model.clone()),
                        thinking_level: projected_level,
                        thinking_selection,
                        service_tier: tier.clone(),
                        ..Default::default()
                    });
                }
            }
        } else {
            let parsed = parse_model_pattern(pattern, models, true);
            thinking = parsed.thinking_level;
            tier = parsed.service_tier.clone();
            if let Some(warning) = &parsed.warning {
                result.diagnostics.push(ModelScopeDiagnostic {
                    code: "invalid-thinking-level",
                    message: warning.clone(),
                    pattern: pattern.clone(),
                });
            }
            if parsed.model.is_some() {
                matches.push(parsed);
            }
        }
        let unresolved = matches.is_empty();
        let mut owned_ids = Vec::new();
        if unresolved {
            result.diagnostics.push(ModelScopeDiagnostic {
                code: "no-match",
                message: format!("No models match pattern \"{pattern}\""),
                pattern: pattern.clone(),
            });
        }
        for parsed in matches {
            let Some(model) = parsed.model else {
                continue;
            };
            let id = format!("{}/{}", model.provider, model.id);
            if claimed.insert(id.clone()) {
                owned_ids.push(id);
                let selection = parsed.thinking_selection.or_else(|| {
                    parsed.thinking_level.map(|level| ThinkingSelection {
                        level,
                        source: ThinkingSelectionSource::Explicit,
                        legacy_variant_id: None,
                    })
                });
                result.scoped_models.push(ScopedModel {
                    model,
                    thinking_level: parsed.thinking_level,
                    thinking_selection: selection,
                    service_tier: parsed.service_tier,
                });
            }
        }
        result.pattern_resolutions.push(PatternResolution {
            pattern: pattern.clone(),
            owned_ids,
            thinking_level: thinking,
            service_tier: tier,
            unresolved,
            is_glob,
        });
    }
    result
}

#[derive(Debug, Default)]
pub struct ResolveCliModelResult {
    pub parsed: ParsedModelResult,
    pub error: Option<String>,
}

pub const DEFAULT_MODEL_PER_PROVIDER: &[(&str, &str)] = &[
    ("alibaba-token-plan", "qwen3.7-max"),
    ("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1"),
    ("ant-ling", "Ring-2.6-1T"),
    ("anthropic", "claude-opus-4-8"),
    ("bai", "gpt-5.6-sol"),
    ("openai", "gpt-6-sol"),
    ("azure-openai-responses", "gpt-5.4"),
    ("chatgpt-subscription", "gpt-6-sol"),
    ("ollama", "qwen3.5:397b"),
    ("cursor", "auto"),
    ("radius", "balanced"),
    ("nvidia", "nvidia/nemotron-3-super-120b-a12b"),
    ("deepseek", "deepseek-v4-pro"),
    ("google", "gemini-3.1-pro-preview"),
    ("google-vertex", "gemini-3.1-pro-preview"),
    ("github-copilot", "gpt-5.4"),
    ("openrouter", "moonshotai/kimi-k2.6"),
    ("vercel-ai-gateway", "zai/glm-5.1"),
    ("opengateway", "moonshotai/kimi-k3"),
    ("xai", "grok-4.7"),
    ("groq", "openai/gpt-oss-120b"),
    ("cerebras", "gpt-oss-120b"),
    ("zai", "glm-5.3"),
    ("zai-coding-cn", "glm-5.3"),
    ("mistral", "devstral-medium-latest"),
    ("minimax", "MiniMax-M2.7"),
    ("minimax-cn", "MiniMax-M2.7"),
    ("moonshotai", "kimi-k2.6"),
    ("moonshotai-cn", "kimi-k2.6"),
    ("huggingface", "moonshotai/Kimi-K2.6"),
    ("fireworks", "accounts/fireworks/models/kimi-k2p6"),
    ("together", "moonshotai/Kimi-K2.6"),
    ("venice", "z-ai-glm-5-3"),
    ("baseten", "zai-org/GLM-5.2"),
    ("opencode", "kimi-k2.6"),
    ("opencode-go", "kimi-k2.6"),
    ("kimi-coding", "kimi-for-coding"),
    ("cloudflare-workers-ai", "@cf/moonshotai/kimi-k2.6"),
    (
        "cloudflare-ai-gateway",
        "workers-ai/@cf/moonshotai/kimi-k2.6",
    ),
    ("qwen-token-plan", "qwen3.7-max"),
    ("qwen-token-plan-cn", "qwen3.7-max"),
    ("qwen-token-plan-individual", "qwen3.8-max"),
    ("xiaomi", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-cn", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-ams", "mimo-v2.5-pro"),
    ("xiaomi-token-plan-sgp", "mimo-v2.5-pro"),
];
fn preferred_available(models: &[Model], is_ambient: impl Fn(&str) -> bool) -> Option<Model> {
    let configured: Vec<Model> = models
        .iter()
        .filter(|model| !is_ambient(&model.provider))
        .cloned()
        .collect();
    let group = if configured.is_empty() {
        models
    } else {
        &configured
    };
    DEFAULT_MODEL_PER_PROVIDER
        .iter()
        .find_map(|(p, id)| {
            group
                .iter()
                .find(|m| m.provider == *p && m.id == *id)
                .cloned()
        })
        .or_else(|| group.first().cloned())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialModelProvenance {
    Cli,
    Scoped,
    Settings,
    ProviderDefault,
    FirstAvailable,
}
/// senpi `model-resolver.ts` `InitialModelProvenance` string union, as `session_start` carries it.
pub fn initial_model_provenance_str(provenance: InitialModelProvenance) -> &'static str {
    match provenance {
        InitialModelProvenance::Cli => "cli",
        InitialModelProvenance::Scoped => "scoped",
        InitialModelProvenance::Settings => "settings",
        InitialModelProvenance::ProviderDefault => "provider-default",
        InitialModelProvenance::FirstAvailable => "first-available",
    }
}
pub struct InitialModelResult {
    pub parsed: ParsedModelResult,
    pub fallback_message: Option<String>,
    pub provenance: InitialModelProvenance,
}
pub struct InitialModelOptions<'a> {
    pub cli_provider: Option<&'a str>,
    pub cli_model: Option<&'a str>,
    pub scoped_models: &'a [ScopedModel],
    pub is_continuing: bool,
    pub default_provider: Option<&'a str>,
    pub default_model_id: Option<&'a str>,
    pub model_thinking_levels: Option<&'a std::collections::HashMap<String, ModelThinkingLevel>>,
}
pub async fn find_initial_model(
    options: InitialModelOptions<'_>,
    runtime: &crate::model_runtime::ModelRuntime,
) -> Result<InitialModelResult, String> {
    if options.cli_provider.is_some() && options.cli_model.is_some() {
        let resolved = resolve_cli_model(options.cli_provider, options.cli_model, None, runtime);
        if let Some(error) = resolved.error {
            return Err(error);
        }
        if resolved.parsed.model.is_some() {
            return Ok(InitialModelResult {
                parsed: resolved.parsed,
                fallback_message: None,
                provenance: InitialModelProvenance::Cli,
            });
        }
    }
    if !options.is_continuing
        && let Some(scoped) = options.scoped_models.first()
    {
        let thinking = options
            .model_thinking_levels
            .and_then(|v| v.get(&format!("{}/{}", scoped.model.provider, scoped.model.id)))
            .copied()
            .or(scoped.thinking_level);
        return Ok(InitialModelResult {
            parsed: ParsedModelResult {
                model: Some(scoped.model.clone()),
                thinking_level: thinking,
                thinking_selection: scoped.thinking_selection.clone(),
                ..Default::default()
            },
            fallback_message: None,
            provenance: InitialModelProvenance::Scoped,
        });
    }
    if let (Some(provider), Some(id)) = (options.default_provider, options.default_model_id)
        && let Some(parsed) =
            resolve_stored_model_reference(provider, id, &runtime.get_models(None))
        && parsed
            .model
            .as_ref()
            .is_some_and(|m| runtime.get_provider_auth_status(&m.provider).configured)
    {
        return Ok(InitialModelResult {
            parsed,
            fallback_message: None,
            provenance: InitialModelProvenance::Settings,
        });
    }
    let available = runtime.get_available(None).await.map_err(|error| error.to_string())?;
    let model = preferred_available(&available, |id| {
        runtime.get_provider_auth_status(id).ambient
    });
    let provenance = if model
        .as_ref()
        .is_some_and(|m| DEFAULT_MODEL_PER_PROVIDER.contains(&(m.provider.as_str(), m.id.as_str())))
    {
        InitialModelProvenance::ProviderDefault
    } else {
        InitialModelProvenance::FirstAvailable
    };
    Ok(InitialModelResult {
        parsed: ParsedModelResult {
            model,
            ..Default::default()
        },
        fallback_message: None,
        provenance,
    })
}
pub async fn restore_model_from_session(
    provider: &str,
    id: &str,
    current: Option<Model>,
    runtime: &crate::model_runtime::ModelRuntime,
) -> Result<InitialModelResult, maho_ai::models::ModelsError> {
    let restored = resolve_stored_model_reference(provider, id, &runtime.get_models(None));
    if let Some(parsed) = &restored
        && parsed
            .model
            .as_ref()
            .is_some_and(|m| runtime.get_provider_auth_status(&m.provider).configured)
    {
        return Ok(InitialModelResult {
            parsed: parsed.clone(),
            fallback_message: None,
            provenance: InitialModelProvenance::Settings,
        });
    }
    let reason = if restored.is_none() {
        "model no longer exists"
    } else {
        "no auth configured"
    };
    let model = if current.is_some() {
        current
    } else {
        preferred_available(&runtime.get_available(None).await?, |id| {
            runtime.get_provider_auth_status(id).ambient
        })
    };
    let message = model.as_ref().map(|m| {
        format!(
            "Could not restore model {provider}/{id} ({reason}). Using {}/{}.",
            m.provider, m.id
        )
    });
    Ok(InitialModelResult {
        parsed: ParsedModelResult {
            model,
            ..Default::default()
        },
        fallback_message: message,
        provenance: InitialModelProvenance::FirstAvailable,
    })
}
pub fn resolve_cli_model(
    provider: Option<&str>,
    pattern: Option<&str>,
    thinking: Option<ModelThinkingLevel>,
    runtime: &crate::model_runtime::ModelRuntime,
) -> ResolveCliModelResult {
    let Some(pattern) = pattern.filter(|p| !p.is_empty()) else {
        return ResolveCliModelResult::default();
    };
    let models = runtime.get_models(None);
    if models.is_empty() {
        return ResolveCliModelResult {
            error: Some(
                "No models available. Check your installation or add models to models.json.".into(),
            ),
            ..Default::default()
        };
    }
    let canonical = |id: &str| {
        models
            .iter()
            .find(|m| m.provider.eq_ignore_ascii_case(id))
            .map(|m| m.provider.clone())
    };
    let mut selected = provider.and_then(canonical);
    let mut model_pattern = pattern;
    if provider.is_some() && selected.is_none() {
        return ResolveCliModelResult {
            error: Some(format!(
                "Unknown provider \"{}\". Use --list-models to see available providers/models.",
                provider.unwrap_or_default()
            )),
            ..Default::default()
        };
    }
    let mut inferred = false;
    if selected.is_none()
        && let Some((id, suffix)) = pattern.split_once('/')
        && let Some(id) = canonical(id)
    {
        selected = Some(id);
        model_pattern = suffix;
        inferred = true;
    }
    if selected.is_none() {
        let exact: Vec<_> = models
            .iter()
            .filter(|m| {
                m.id.eq_ignore_ascii_case(pattern)
                    || format!("{}/{}", m.provider, m.id).eq_ignore_ascii_case(pattern)
            })
            .collect();
        if exact.len() == 1 {
            return ResolveCliModelResult {
                parsed: ParsedModelResult {
                    model: Some(exact[0].clone()),
                    ..Default::default()
                },
                error: None,
            };
        }
        if exact.len() > 1 {
            let authenticated: Vec<_> = exact
                .iter()
                .filter(|m| runtime.get_provider_auth_status(&m.provider).configured)
                .collect();
            if authenticated.len() == 1 {
                return ResolveCliModelResult {
                    parsed: ParsedModelResult {
                        model: Some((*authenticated[0]).clone()),
                        ..Default::default()
                    },
                    error: None,
                };
            }
            let mut ids: Vec<_> = exact
                .iter()
                .map(|m| format!("{}/{}", m.provider, m.id))
                .collect();
            ids.sort();
            let hint = if authenticated.is_empty() {
                "No matching provider is authenticated."
            } else {
                "More than one matching provider is authenticated."
            };
            return ResolveCliModelResult {
                error: Some(format!(
                    "Model \"{pattern}\" is ambiguous across providers: {}. {hint} Use --provider or provider/model.",
                    ids.join(", ")
                )),
                ..Default::default()
            };
        }
    }
    if let Some(id) = &selected
        && provider.is_some()
        && pattern
            .to_lowercase()
            .starts_with(&format!("{id}/").to_lowercase())
    {
        model_pattern = &pattern[id.len() + 1..];
    }
    let candidates: Vec<_> = models
        .iter()
        .filter(|m| selected.as_ref().is_none_or(|p| *p == m.provider))
        .cloned()
        .collect();
    let parsed = parse_model_pattern(model_pattern, &candidates, false);
    if let Some(model) = &parsed.model {
        // If provider inference matched an unauthenticated provider/model pair, prefer one exact
        // raw model-id match that is authenticated (e.g. a gateway model whose literal id is
        // "xiaomi/mimo-v2.5-pro").
        if inferred && !runtime.get_provider_auth_status(&model.provider).configured {
            let authenticated: Vec<_> = models
                .iter()
                .filter(|m| {
                    m.id.eq_ignore_ascii_case(pattern)
                        && !(m.id == model.id && m.provider == model.provider)
                })
                .filter(|m| runtime.get_provider_auth_status(&m.provider).configured)
                .collect();
            if authenticated.len() == 1 {
                return ResolveCliModelResult {
                    parsed: ParsedModelResult {
                        model: Some(authenticated[0].clone()),
                        service_tier: parsed.service_tier,
                        ..Default::default()
                    },
                    error: None,
                };
            }
        }
        return ResolveCliModelResult {
            parsed,
            error: None,
        };
    }
    let ParsedModelResult {
        warning,
        service_tier,
        ..
    } = parsed;
    if inferred {
        let parsed = parse_model_pattern(pattern, &models, false);
        if parsed.model.is_some() {
            return ResolveCliModelResult {
                parsed,
                error: None,
            };
        }
    }
    if let Some(id) = &selected
        && let Some(base) = candidates.first()
    {
        let (model_id, level) = if thinking.is_none() {
            model_pattern
                .rsplit_once(':')
                .and_then(|(prefix, suffix)| {
                    ModelThinkingLevel::parse(suffix).map(|l| (prefix, Some(l)))
                })
                .unwrap_or((model_pattern, None))
        } else {
            (model_pattern, None)
        };
        let base = DEFAULT_MODEL_PER_PROVIDER
            .iter()
            .find(|(p, _)| *p == id.as_str())
            .and_then(|(_, default)| candidates.iter().find(|m| m.id == *default))
            .unwrap_or(base);
        let mut model = base.clone();
        model.id = model_id.into();
        model.name = model_id.into();
        if thinking
            .or(level)
            .is_some_and(|l| l != ModelThinkingLevel::Off)
        {
            model.reasoning = true;
        }
        let not_found =
            format!("Model \"{model_id}\" not found for provider \"{id}\". Using custom model id.");
        let warning = Some(match warning {
            Some(warning) => format!("{warning} {not_found}"),
            None => not_found,
        });
        return ResolveCliModelResult {
            parsed: ParsedModelResult {
                model: Some(model),
                thinking_level: level,
                service_tier,
                warning,
                ..Default::default()
            },
            error: None,
        };
    }
    let display = selected
        .as_ref()
        .map_or_else(|| pattern.to_owned(), |id| format!("{id}/{model_pattern}"));
    ResolveCliModelResult {
        parsed: ParsedModelResult {
            service_tier,
            warning,
            ..Default::default()
        },
        error: Some(format!(
            "Model \"{display}\" not found. Use --list-models to see available models."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn automatic_defaults_prefer_configured_providers_but_keep_ambient_fallback() {
        let ambient = model("amazon-bedrock", "us.anthropic.claude-opus-4-6-v1");
        let configured = model("custom", "configured");
        let is_ambient = |provider: &str| provider == "amazon-bedrock";
        assert_eq!(
            preferred_available(&[ambient.clone(), configured.clone()], is_ambient)
                .expect("configured")
                .provider,
            "custom"
        );
        assert_eq!(
            preferred_available(&[ambient], is_ambient)
                .expect("ambient fallback")
                .provider,
            "amazon-bedrock"
        );
        assert!(preferred_available(&[], is_ambient).is_none());
    }

    fn model(provider: &str, id: &str) -> Model {
        let mut model = maho_ai::providers::all::get_builtin_models("openai").remove(0);
        model.provider = provider.into();
        model.id = id.into();
        model.name = id.into();
        model
    }
    #[test]
    fn scope_first_pattern_owns_deduplicated_models() {
        let models = vec![model("p", "sol"), model("q", "sol")];
        let result = resolve_model_scope_from_models(&["p/*:high".into(), "*/sol".into()], &models);
        assert_eq!(result.scoped_models.len(), 2);
        assert_eq!(result.pattern_resolutions[1].owned_ids, ["q/sol"]);
        assert_eq!(
            result.scoped_models[0].thinking_level,
            Some(ModelThinkingLevel::High)
        );
    }
    #[test]
    fn ultrafast_is_a_service_tier_suffix() {
        let parsed = parse_model_pattern("sol:ultrafast", &[model("p", "sol")], false);
        assert_eq!(parsed.service_tier.as_deref(), Some("ultrafast"));
    }
    #[test]
    fn literal_colon_id_precedes_thinking_decorator() {
        let models = vec![model("p", "sol:high")];
        let parsed = parse_model_pattern("sol:high", &models, true);
        assert_eq!(parsed.model.expect("model").id, "sol:high");
        assert_eq!(parsed.thinking_level, None);
    }
    #[test]
    fn provider_glob_does_not_cross_slashes() {
        let result = resolve_model_scope_from_models(
            &["p/*".into()],
            &[model("p", "sol"), model("p", "org/sol")],
        );
        assert_eq!(result.pattern_resolutions[0].owned_ids, ["p/sol"]);
    }
    #[test]
    fn brace_glob_matches_each_alternative() {
        let result = resolve_model_scope_from_models(
            &["{p,q}/S*".into()],
            &[model("p", "sol"), model("q", "sol"), model("r", "sol")],
        );
        assert_eq!(result.pattern_resolutions[0].owned_ids, ["p/sol", "q/sol"]);
    }
    #[test]
    fn glob_projects_cursor_legacy_alias_selection() {
        let result = resolve_model_scope_from_models(
            &["cursor/claude-4.5-opus-h*".into()],
            &[model("cursor", "claude-4.5-opus")],
        );
        assert_eq!(
            result.pattern_resolutions[0].owned_ids,
            ["cursor/claude-4.5-opus"]
        );
        let selection = result.scoped_models[0]
            .thinking_selection
            .clone()
            .expect("alias selection");
        assert_eq!(
            (
                selection.level,
                selection.source,
                selection.legacy_variant_id.as_deref()
            ),
            (
                ModelThinkingLevel::High,
                ThinkingSelectionSource::LegacyVariant,
                Some("claude-4.5-opus-high")
            )
        );
    }
    #[test]
    fn inferred_unauthenticated_provider_prefers_authenticated_raw_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("models.json");
        std::fs::write(&path,r#"{"providers":{"xprov":{"api":"openai-completions","baseUrl":"http://localhost/v1","models":[{"id":"mimo"}]},"gw":{"api":"openai-completions","baseUrl":"http://localhost/v1","apiKey":"fixture-key","models":[{"id":"xprov/mimo"}]}}}"#).expect("config");
        let runtime = crate::model_runtime::ModelRuntime::create_sync(
            crate::model_runtime::CreateModelRuntimeOptions {
                models_path: Some(path),
                providers: Some(Vec::new()),
                ..Default::default()
            },
        ).expect("model runtime");
        let resolved = resolve_cli_model(None, Some("xprov/mimo"), None, &runtime);
        let model = resolved.parsed.model.expect("model");
        assert_eq!(
            (model.provider.as_str(), model.id.as_str()),
            ("gw", "xprov/mimo")
        );
    }
    #[test]
    fn unknown_scope_is_retained_as_unresolved() {
        let result = resolve_model_scope_from_models(&["missing".into()], &[model("p", "sol")]);
        assert!(result.pattern_resolutions[0].unresolved);
        assert_eq!(result.diagnostics[0].code, "no-match");
    }
}
