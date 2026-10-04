//! maho-core: port of senpi packages/coding-agent/src/core.
//!
//! Module map (one senpi source file per module, same names and order):
//!
//! todo 16 (this todo): brand, config, auth_storage, credential_accounts, credential_pool/,
//!   defaults, diagnostics, event_bus, exec, lockfile_policy, messages, nearest_parent_config,
//!   output_guard, paths, resolve_config_value, session_manager, session_resident_store,
//!   session_title_generator, settings_manager, text, trust_manager.
//! todo 17: model_runtime, model_resolver, model_registry, model_config, provider_composer,
//!   provider_api_key_auth, models_json_migration, auth_providers, auth_guidance, cache_stats,
//!   retry_fallback.
//! todo 19: dynamic_prompt, system_prompt, skills, skill_discovery, skill_invocation,
//!   prompt_templates, resource_loader, discovered_resource_scope, compaction, package_manager,
//!   keybindings.
//! todo 21 (remainder): agent_session, agent_session_runtime, agent_session_services, sdk,
//!   session_discovery, session_entry_materializer, session_write_reservation, session_log,
//!   session_record, session_export, session_summary, session_cwd, settings_shapes,
//!   settings_public_types, settings_overrides, terminal_settings, compaction_settings_access,
//!   compaction_settings_resolver, http_dispatcher, and the rest of the senpi core tree.

pub mod auth_guidance;
pub mod auth_providers;
pub mod auth_storage;
pub mod brand;
pub mod cache_stats;
pub mod compaction;
pub mod config;
pub mod credential_accounts;
pub mod credential_pool;
pub mod defaults;
pub mod diagnostics;
pub mod discovered_resource_scope;
pub mod dynamic_prompt;
pub mod event_bus;
pub mod exec;
pub mod frontmatter;
pub mod high_reasoning_warning;
pub mod keybindings;
pub mod lockfile_policy;
pub mod messages;
pub mod model_config;
pub mod model_config_schema;
pub mod model_registry;
pub mod model_resolver;
pub mod model_runtime;
pub mod models_json_migration;
pub mod nearest_parent_config;
pub mod output_guard;
pub mod package_manager;
pub mod paths;
pub mod prompt_templates;
pub mod provider_api_key_auth;
pub mod provider_composer;
pub mod resolve_config_value;
pub mod resource_loader;
pub mod retry_fallback;
pub mod session_manager;
pub mod session_resident_store;
pub mod session_title_generator;
pub mod settings_manager;
pub mod skill_discovery;
pub mod skill_invocation;
pub mod skills;
pub mod source_info;
pub mod system_prompt;
pub mod text;
pub mod trust_manager;

pub mod agent_abort_provenance;
pub mod agent_session_runtime;
pub mod agent_session_services;
pub mod agent_session;
pub mod agent_settled_delivery;
pub mod auth_provider_key_migration;
pub mod changelog_source;
pub mod compaction_settings_access;
pub mod compaction_settings_resolver;
pub mod cursor_exec_bridge_session;
pub mod cursor_exec_bridge;
pub mod cursor_history_admission;
pub mod edited_assistant_message;
pub mod edited_user_message;
pub mod engine_build_identity;
pub mod experimental;
pub mod footer_data_provider;
pub mod generated_shim_banner;
pub mod hidden_stdout_log;
pub mod http_dispatcher;
pub mod manual_continue;
pub mod models_store;
pub mod package_identity;
pub mod pi_manifest;
pub mod project_trust;
pub mod prompt_cache_budget;
pub mod provider_account_events;
pub mod provider_attribution;
pub mod provider_concurrency;
pub mod provider_display_names;
pub mod provider_header_auth;
pub mod provider_timeout_retry;
pub mod remote_catalog_merge;
pub mod remote_catalog_provider;
pub mod resolve_config_command;
pub mod resolved_paths_memo;
pub mod runtime_credentials;
pub mod sdk;
pub mod sensitive_output;
pub mod session_activity;
pub mod session_cwd;
pub mod session_discovery;
pub mod session_entry_materializer;
pub mod session_export;
pub mod session_log;
pub mod session_record;
pub mod session_sidecar_store;
pub mod session_summary_cache;
pub mod session_summary_lru;
pub mod session_summary;
pub mod session_work_barrier;
pub mod session_write_reservation;
pub mod settings_diagnostics;
pub mod settings_overrides;
pub mod settings_public_types;
pub mod settings_shapes;
pub mod shared_host_policy;
pub mod slash_commands;
pub mod startup_branch_join;
pub mod telemetry;
pub mod terminal_settings;
pub mod thinking_levels;
pub mod timings;
pub mod tool_call_display_name;
pub mod usage_totals;
pub mod export_html;

/// Test-only helpers shared by maho-core's unit tests.
#[cfg(test)]
pub(crate) mod test_support {
    /// A temp root created outside the real HOME so the ancestor walks that read project
    /// context files, the nearest parent config dir and project trust never escape into the
    /// developer's home directory. `tempfile`'s default honours `TMPDIR`, which a verification
    /// harness may point inside HOME; anchoring on `/tmp` keeps every ancestor marker-free.
    pub(crate) fn isolated_tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("maho-core-isolated-")
            .tempdir_in("/tmp")
            .expect("isolated temp root under /tmp")
    }
}

pub use auth_storage::{AuthStorage, CredentialKind, ReadOnlyAuthStorage, read_stored_credential};
pub use brand::{BRAND_ENV_VAR, BrandProfile, brand_profile, env_value, parse_brand_profile};
pub use config::{app_command, app_name, app_title, config_dir_name, get_agent_dir, get_sessions_dir};
pub use credential_accounts::{
    CredentialAccountSource, CredentialAccountSummary, get_credential_accounts, pin_credential_account,
    remove_credential_account, rename_credential_account,
};
pub use credential_pool::state_store::{CredentialSlotRepository, CredentialSlotState, SlotHealth, slot_health};
pub use defaults::{DEFAULT_THINKING_LEVEL, THINKING_LEVEL_OPTIONS};
pub use event_bus::{EXTENSION_RPC_EVENT_CHANNEL, EventBus};
pub use messages::{
    BRANCH_SUMMARY_PREFIX, BRANCH_SUMMARY_SUFFIX, COMPACTION_SUMMARY_PREFIX, COMPACTION_SUMMARY_SUFFIX,
    convert_to_llm, convert_to_llm_for_transport, elide_old_images,
};
pub use resolve_config_value::{
    clear_config_value_cache, is_command_config_value, resolve_config_value, resolve_config_value_or_throw,
    resolve_headers_or_throw,
};
pub use session_manager::{
    CURRENT_SESSION_VERSION, NewSessionOptions, SessionContext, SessionManager, SessionTreeNode, UsageTotals,
    assert_valid_session_id, build_context_entries, build_session_context, build_session_path,
    find_most_recent_session, get_default_session_dir, load_entries_from_file, migrate_session_entries,
    parse_session_entries, serialize_entry,
};
pub use session_resident_store::{
    RESIDENT_STRING_PREFIX, ResidentStringStore, ResidentStringStoreOptions, ResidentStoreStats,
};
pub use session_title_generator::{humanize_provider_error, parse_session_title, should_skip_session_title};
pub use prompt_templates::{
    LoadPromptTemplatesOptions, PromptTemplate, PromptTemplateExpansion, expand_prompt_template,
    expand_prompt_template_with_metadata, load_prompt_templates, parse_command_args, substitute_args,
};
pub use provider_account_events::{
    ProviderAccountEvent, ProviderAccountEvents, emit_provider_account_failover,
    emit_provider_accounts_changed, provider_account_events, subscribe_provider_account_events,
};
pub use source_info::{
    SourceInfo, SourceOrigin, SourceScope, SyntheticSourceInfoOptions, create_source_info,
    create_synthetic_source_info,
};
pub use skills::{
    FileReadTool, LoadSkillsFromDirOptions, LoadSkillsOptions, LoadSkillsResult, MAX_DESCRIPTION_LENGTH,
    MAX_NAME_LENGTH, Skill, format_skills_for_prompt, load_skill_from_file, load_skills,
    load_skills_from_dir, validate_description, validate_name,
};
pub use skill_invocation::{
    MAX_SKILL_EXPANSIONS_PER_PROMPT, MAX_SKILL_INVOCATION_TOKENS_PER_PROMPT, ParsedSkillBlock,
    ParsedSkillBlockSkill, SkillInvocationPromptSkill, SkillInvocationSyntax, SkillInvocationToken,
    format_skill_invocation_prompt, parse_skill_block, parse_skill_invocation_tokens,
    remove_skill_invocation_tokens,
};
pub use skill_discovery::{SkillDiscoveryMode, collect_auto_skill_entries, collect_skill_entries, read_skill_markdown_source};
pub use system_prompt::{BuildSystemPromptOptions, ContextFile, build_system_prompt, get_eval_only_grep_guideline};
pub use settings_manager::{Settings, SettingsManager, SettingsScope, parse_settings_json};
pub use trust_manager::{ProjectTrustDecision, ProjectTrustStore, ProjectTrustUpdate};
