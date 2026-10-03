use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolSearchSource { Mcp, Extension }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSearchDocument {
    pub name: String,
    pub label: String,
    pub aliases: Vec<String>,
    pub description: Option<String>,
    pub search_text: Option<String>,
    pub keywords: Vec<String>,
    pub source: ToolSearchSource,
    pub group: String,
    pub owner_label: String,
    pub registration_id: String,
}
