//! Compatibility re-export of the shared tool-search native surface (upstream
//! `mcp/expose/native-search.ts`); MCP contributes only its setting gate.
//!
//! Upstream `mcp/expose/native-search.ts` re-exports three type names
//! (`AnthropicNativeAdapterDeps`, `AnthropicNativeInjectionConfig`,
//! `NativeToolDefinition`) and five values. The owner crate
//! (`crates/builtins/maho-ext-tool-search`, todo 28) already exports every name here
//! except `AnthropicNativeAdapterDeps` (upstream `tool-search/native-search.ts:114`).
//! That type is the one outstanding owner contract: the owner must add and export from
//! `maho_ext_tool_search::native_search`
//! `pub struct AnthropicNativeAdapterDeps` — a deps object extending
//! `AnthropicNativeInjectionConfig` that carries a provider/config gate `enabled()` and an
//! optional `onFallback(reason)` invoked once when a 400 forces the local-search fallback —
//! and the constructor `AnthropicNativeToolSearchAdapter::new(deps)` that upstream keys on it.
//! The MCP half of the contract is `McpService::native_tool_search_gate()` (see
//! src/service.rs): the host binds that gate into the shared tool-search adapter so `enabled()`
//! reads the resolved `settings.nativeToolSearch`. Until the owner adds the type, the native
//! adapter keeps taking the resolved flag per call and returning the fallback reason instead of
//! holding a deps object, so no local substitute is introduced and this name cannot be
//! re-exported yet.
pub use maho_ext_tool_search::native_search::{add_anthropic_native_tool_search,build_tool_reference_blocks,AnthropicNativeInjectionConfig,AnthropicNativeToolSearchAdapter,ANTHROPIC_MAX_TOOLS,ANTHROPIC_TOOL_SEARCH_NAME,ANTHROPIC_TOOL_SEARCH_TYPE,NativeToolDefinition};
pub use maho_ext_tool_search::native_support::supports_anthropic_native_tool_search;
