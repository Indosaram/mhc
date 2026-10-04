#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MultiAgentModeVariant01 {
    #[serde(rename = "custom")]
    pub custom: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MultiAgentModeVariant12 {
    #[serde(rename = "explicitRequestOnly")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MultiAgentModeVariant23 {
    #[serde(rename = "proactive")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum MultiAgentMode {
    Variant0(Box<MultiAgentModeVariant01>),
    Variant1(Box<MultiAgentModeVariant12>),
    Variant2(Box<MultiAgentModeVariant23>),
}
