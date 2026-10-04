#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AskForApprovalVariant01 {
    #[serde(rename = "untrusted")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AskForApprovalVariant12 {
    #[serde(rename = "on-request")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AskForApprovalVariant23Granular4 {
    #[serde(rename = "sandbox_approval")]
    pub sandbox_approval: bool,
    #[serde(rename = "rules")]
    pub rules: bool,
    #[serde(rename = "skill_approval")]
    pub skill_approval: bool,
    #[serde(rename = "request_permissions")]
    pub request_permissions: bool,
    #[serde(rename = "mcp_elicitations")]
    pub mcp_elicitations: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AskForApprovalVariant23 {
    #[serde(rename = "granular")]
    pub granular: AskForApprovalVariant23Granular4,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AskForApprovalVariant35 {
    #[serde(rename = "never")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum AskForApproval {
    Variant0(Box<AskForApprovalVariant01>),
    Variant1(Box<AskForApprovalVariant12>),
    Variant2(Box<AskForApprovalVariant23>),
    Variant3(Box<AskForApprovalVariant35>),
}
