#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SubAgentSourceVariant01 {
    #[serde(rename = "review")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SubAgentSourceVariant12 {
    #[serde(rename = "compact")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubAgentSourceVariant23ThreadSpawn4 {
    #[serde(rename = "parent_thread_id")]
    pub parent_thread_id: Box<crate::app_server::protocol::generated::thread_id::ThreadId>,
    #[serde(rename = "depth")]
    pub depth: f64,
    #[serde(rename = "agent_path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub agent_path: Option<Box<crate::app_server::protocol::generated::agent_path::AgentPath>>,
    #[serde(rename = "agent_nickname", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub agent_nickname: Option<String>,
    #[serde(rename = "agent_role", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub agent_role: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubAgentSourceVariant23 {
    #[serde(rename = "thread_spawn")]
    pub thread_spawn: SubAgentSourceVariant23ThreadSpawn4,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SubAgentSourceVariant35 {
    #[serde(rename = "memory_consolidation")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubAgentSourceVariant46 {
    #[serde(rename = "other")]
    pub other: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum SubAgentSource {
    Variant0(Box<SubAgentSourceVariant01>),
    Variant1(Box<SubAgentSourceVariant12>),
    Variant2(Box<SubAgentSourceVariant23>),
    Variant3(Box<SubAgentSourceVariant35>),
    Variant4(Box<SubAgentSourceVariant46>),
}
