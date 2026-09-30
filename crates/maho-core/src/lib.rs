//! maho-core: port of senpi packages/coding-agent/src/core (todo 16: persistence, settings,
//! brand, auth storage, trust).
//!
//! Module map (one senpi source file per module, same names and order):
//!
//! todo 16 (this todo): brand, defaults, diagnostics, event_bus, exec, messages, session_manager,
//!   text, config/paths/nearest-parent-config/lockfile-policy helpers.
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

pub mod brand;
pub mod config;
pub mod defaults;
pub mod diagnostics;
pub mod event_bus;
pub mod exec;
pub mod lockfile_policy;
pub mod output_guard;
pub mod resolve_config_value;
pub mod messages;
pub mod nearest_parent_config;
pub mod paths;
pub mod session_manager;
pub mod settings_manager;
pub mod session_resident_store;
pub mod trust_manager;
pub mod text;

pub use brand::{BRAND_ENV_VAR, BrandProfile, brand_profile, env_value, parse_brand_profile};
pub use config::{app_command, app_name, app_title, config_dir_name, get_agent_dir, get_sessions_dir};
pub use defaults::{DEFAULT_THINKING_LEVEL, THINKING_LEVEL_OPTIONS};
pub use messages::{
    BRANCH_SUMMARY_PREFIX, BRANCH_SUMMARY_SUFFIX, COMPACTION_SUMMARY_PREFIX, COMPACTION_SUMMARY_SUFFIX,
    convert_to_llm, convert_to_llm_for_transport, elide_old_images,
};
pub use settings_manager::{Settings, SettingsManager, SettingsScope, parse_settings_json};
pub use session_resident_store::{RESIDENT_STRING_PREFIX, ResidentStringStore, ResidentStringStoreOptions, ResidentStoreStats};
pub use trust_manager::{ProjectTrustDecision, ProjectTrustStore, ProjectTrustUpdate};
pub use session_manager::{
    CURRENT_SESSION_VERSION, NewSessionOptions, SessionContext, SessionManager, SessionTreeNode,
    UsageTotals, assert_valid_session_id, build_context_entries, build_session_context, build_session_path,
    find_most_recent_session, get_default_session_dir, load_entries_from_file, migrate_session_entries,
    parse_session_entries, serialize_entry,
};
