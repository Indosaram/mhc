#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SessionSourceVariant01 {
    #[serde(rename = "cli")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SessionSourceVariant12 {
    #[serde(rename = "vscode")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SessionSourceVariant23 {
    #[serde(rename = "exec")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SessionSourceVariant34 {
    #[serde(rename = "mcp")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionSourceVariant45 {
    #[serde(rename = "custom")]
    pub custom: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionSourceVariant56 {
    #[serde(rename = "internal")]
    pub internal: Box<crate::app_server::protocol::generated::internal_session_source::InternalSessionSource>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionSourceVariant67 {
    #[serde(rename = "subagent")]
    pub subagent: Box<crate::app_server::protocol::generated::sub_agent_source::SubAgentSource>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SessionSourceVariant78 {
    #[serde(rename = "unknown")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum SessionSource {
    Variant0(Box<SessionSourceVariant01>),
    Variant1(Box<SessionSourceVariant12>),
    Variant2(Box<SessionSourceVariant23>),
    Variant3(Box<SessionSourceVariant34>),
    Variant4(Box<SessionSourceVariant45>),
    Variant5(Box<SessionSourceVariant56>),
    Variant6(Box<SessionSourceVariant67>),
    Variant7(Box<SessionSourceVariant78>),
}
