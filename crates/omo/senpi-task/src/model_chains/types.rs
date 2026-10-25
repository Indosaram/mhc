use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChainRungState {
    Primary,
    Fallback,
    Excluded,
    NoLogin,
    NotReached,
}
/// `built-in`, `omo.json`, or `category:<name>`.
pub type ChainRungSource = String;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainRung {
    pub model: String,
    pub source: ChainRungSource,
    pub state: ChainRungState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<String>>,
    #[serde(rename = "excludedBy", skip_serializing_if = "Option::is_none")]
    pub excluded_by: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChainEntry {
    pub kind: String,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    pub runtime: Vec<String>,
    pub rungs: Vec<ChainRung>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChainsReport {
    #[serde(rename = "excludeModels")]
    pub exclude_models: Vec<String>,
    pub categories: Vec<ModelChainEntry>,
    pub agents: Vec<ModelChainEntry>,
}
