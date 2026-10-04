//! CLI-owned shared tool-search service and its MCP binding.
//!
//! Ports the host half of senpi `core/extensions/builtin/tool-search/service.ts`
//! (`getToolSearchService` / `installScopedToolSearchService`) and the MCP binding in
//! `tool-search/native-search.ts` (`installMcpNativeToolSearchGate`): one `ToolSearchService` per
//! session, handed to the MCP lifecycle so the MCP native gate and the Anthropic adapter read the
//! same resolved state.

use std::sync::Arc;

use maho_ext_api::{ExtensionActions, ExtensionRuntime};
use maho_ext_tool_search::service::ToolSearchService;

/// The exact mutex type lane-22's `register_mcp_lifecycle` consumes.
pub type SharedToolSearchService = Arc<tokio::sync::Mutex<ToolSearchService>>;

/// The one shared tool-search service the CLI owns for a session.
pub struct SharedToolSearch {
    service: SharedToolSearchService,
}

impl SharedToolSearch {
    pub fn new(runtime: ExtensionRuntime, actions: Arc<dyn ExtensionActions>) -> Self {
        Self { service: Arc::new(tokio::sync::Mutex::new(ToolSearchService::new(runtime, actions))) }
    }

    pub fn service(&self) -> &SharedToolSearchService {
        &self.service
    }

    pub fn into_mcp_argument(self) -> SharedToolSearchService {
        self.service
    }
}

/// The canonical instance `register_mcp_lifecycle(api, registry, owner, tool_search)` takes.
pub fn mcp_tool_search_argument(shared: &SharedToolSearch) -> SharedToolSearchService {
    Arc::clone(shared.service())
}
