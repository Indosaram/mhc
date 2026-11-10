//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "arity.rs"]
mod arity;
#[path = "bash_command_segments.rs"]
mod bash_command_segments;
#[path = "canonical_identity.rs"]
mod canonical_identity;
#[path = "cli_source.rs"]
mod cli_source;
#[path = "config.rs"]
mod config;
#[path = "context_support.rs"]
mod context_support;
#[path = "denial_is_final.rs"]
mod denial_is_final;
#[path = "dispatch.rs"]
mod dispatch;
#[path = "dispatch_metadata.rs"]
mod dispatch_metadata;
#[path = "dispatch_policy.rs"]
mod dispatch_policy;
#[path = "events.rs"]
mod events;
#[path = "external.rs"]
mod external;
#[path = "external_paths.rs"]
mod external_paths;
#[path = "guarded.rs"]
mod guarded;
#[path = "integration_source.rs"]
mod integration_source;
#[path = "integration_suite.rs"]
mod integration_suite;
#[path = "multi_mode.rs"]
mod multi_mode;
#[path = "native_persistence.rs"]
mod native_persistence;
#[path = "non_interactive.rs"]
mod non_interactive;
#[path = "parser_cases.rs"]
mod parser_cases;
#[path = "parser_contract_source.rs"]
mod parser_contract_source;
#[path = "parsers.rs"]
mod parsers;
#[path = "preset_source.rs"]
mod preset_source;
#[path = "prompt_binding.rs"]
mod prompt_binding;
#[path = "read_permission.rs"]
mod read_permission;
#[path = "registration.rs"]
mod registration;
#[path = "resume_parsers.rs"]
mod resume_parsers;
#[path = "rules.rs"]
mod rules;
#[path = "service.rs"]
mod service;
#[path = "service_source.rs"]
mod service_source;
#[path = "settings.rs"]
mod settings;
#[path = "storage.rs"]
mod storage;
