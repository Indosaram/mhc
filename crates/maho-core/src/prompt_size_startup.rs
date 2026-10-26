//! Metadata-only startup inventory; never connects to MCP servers.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InventoryNote {
    pub names: Vec<String>,
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSizeStartupNotes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<InventoryNote>,
    pub third_party_session_start: InventoryNote,
}
pub fn prompt_size_startup_notes(
    paths: &[String],
    extension_names: &HashMap<String, String>,
    configured_servers: Option<Vec<String>>,
    sizes_only_startup: bool,
) -> PromptSizeStartupNotes {
    PromptSizeStartupNotes {
        third_party_session_start: InventoryNote { names: paths.iter().map(|p| extension_names.get(p).cloned().unwrap_or_else(|| "extension".into())).collect(), note: "these third-party session_start hooks ran during startup and may have used the network".into(), count: None },
        mcp: if sizes_only_startup { configured_servers.map(|mut names| { names.sort(); InventoryNote { count: Some(names.len()), names, note: "not attached; tool sizes exclude them".into() } }) } else { None },
    }
}
