//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "compaction_estimate.rs"]
mod compaction_estimate;
#[path = "context_selection.rs"]
mod context_selection;
#[path = "extension_lock_release.rs"]
mod extension_lock_release;
#[path = "sdk_accounts.rs"]
mod sdk_accounts;
#[path = "sdk_async_hook_contract.rs"]
mod sdk_async_hook_contract;
#[path = "sdk_async_wire.rs"]
mod sdk_async_wire;
#[path = "sdk_base_tool_context.rs"]
mod sdk_base_tool_context;
#[path = "sdk_custom_tools.rs"]
mod sdk_custom_tools;
#[path = "sdk_extension_resources.rs"]
mod sdk_extension_resources;
#[path = "sdk_filesystem_policy.rs"]
mod sdk_filesystem_policy;
#[path = "sdk_minimal_resources.rs"]
mod sdk_minimal_resources;
#[path = "sdk_model_auth.rs"]
mod sdk_model_auth;
#[path = "sdk_native_renderers.rs"]
mod sdk_native_renderers;
#[path = "sdk_permission_admission.rs"]
mod sdk_permission_admission;
#[path = "sdk_prompt_selection.rs"]
mod sdk_prompt_selection;
#[path = "sdk_recommended_models.rs"]
mod sdk_recommended_models;
#[path = "sdk_rpc_options.rs"]
mod sdk_rpc_options;
#[path = "sdk_services.rs"]
mod sdk_services;
#[path = "sdk_tool_selection.rs"]
mod sdk_tool_selection;
#[path = "sdk_tool_updates.rs"]
mod sdk_tool_updates;
#[path = "session_qa.rs"]
mod session_qa;
