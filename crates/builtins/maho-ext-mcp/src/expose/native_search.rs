//! Compatibility re-export of the shared tool-search native surface (upstream
//! `mcp/expose/native-search.ts`); MCP contributes only its setting gate.
//!
//! Cross-owner gap (crates/builtins/maho-ext-tool-search, todo 28): upstream's
//! `native-search.ts` also declares `AnthropicNativeAdapterDeps` (a deps-shaped adapter with
//! `enabled()`/`onFallback()`), which this module re-exports. The native adapter instead takes
//! the resolved `enabled` flag and returns the fallback reason per call, so the type has no
//! local counterpart yet; it cannot be re-exported until the tool-search owner adds it. The MCP
//! half of that contract is `McpService::native_tool_search_gate()` (see src/service.rs), which
//! the host reads per request to supply that flag.
pub use maho_ext_tool_search::native_search::{add_anthropic_native_tool_search,build_tool_reference_blocks,AnthropicNativeInjectionConfig,AnthropicNativeToolSearchAdapter,ANTHROPIC_MAX_TOOLS,ANTHROPIC_TOOL_SEARCH_NAME,ANTHROPIC_TOOL_SEARCH_TYPE,NativeToolDefinition};
pub use maho_ext_tool_search::native_support::supports_anthropic_native_tool_search;
