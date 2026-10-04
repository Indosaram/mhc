//! Compatibility re-export of the shared tool-search native surface (upstream
//! `mcp/expose/native-search.ts`); MCP contributes only its setting gate.
//!
//! Upstream `mcp/expose/native-search.ts` re-exports three type names
//! (`AnthropicNativeAdapterDeps`, `AnthropicNativeInjectionConfig`,
//! `NativeToolDefinition`) and five values. The owner crate
//! (`crates/builtins/maho-ext-tool-search`) now exports all three type names,
//! including `AnthropicNativeAdapterDeps` (upstream `tool-search/native-search.ts:114`),
//! plus `AnthropicNativeToolSearchAdapter::new(deps)` and the deps-based
//! `apply_before_request_with_deps`/`on_fallback` path. The MCP half of the contract is
//! `McpService::native_tool_search_gate()` (see src/service.rs): the host binds that gate
//! into the deps' `enabled()` closure and routes the adapter's `on_fallback` to
//! `ToolSearchService::note_native_injection_failure`.
pub use maho_ext_tool_search::native_search::{add_anthropic_native_tool_search,build_tool_reference_blocks,AnthropicNativeAdapterDeps,AnthropicNativeInjectionConfig,AnthropicNativeToolSearchAdapter,ANTHROPIC_MAX_TOOLS,ANTHROPIC_TOOL_SEARCH_NAME,ANTHROPIC_TOOL_SEARCH_TYPE,NativeToolDefinition};
pub use maho_ext_tool_search::native_support::supports_anthropic_native_tool_search;
