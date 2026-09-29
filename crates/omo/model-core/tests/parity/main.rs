//! Translations of the `@oh-my-opencode/model-core` bun:test suites (one module per TS file).

mod support;

mod category_routing_policy;
mod context_limit_resolver;
mod gpt_5_6_copilot_resolution;
mod luna_deepseek_chain_policy;
mod model_availability;
mod model_capabilities;
mod model_capabilities_bundled_snapshot;
mod model_capabilities_heuristics;
mod model_capabilities_openai_fast_aliases;
mod model_capabilities_snapshot;
mod model_capabilities_suffixed_provider_lookup;
mod model_capability_aliases;
mod model_capability_guardrails;
mod model_error_classifier;
mod model_error_classifier_openai_usage_limit;
mod model_family_detectors;
mod model_format_normalizer;
mod model_normalization;
mod model_requirements_agents;
mod model_requirements_categories;
mod model_requirements_deprecated_routing;
mod model_requirements_invariants;
mod model_resolution_pipeline;
mod model_resolver;
mod model_resolver_provider_scope;
mod model_settings_compatibility;
mod model_string_parser;
mod parse_model_suggestion;
mod provider_exhaustion_fallback_policy;
mod provider_model_id_transform;
mod reasoning_level;
mod runtime_fallback_auto_retry_signal;
mod runtime_fallback_error_classifier;
