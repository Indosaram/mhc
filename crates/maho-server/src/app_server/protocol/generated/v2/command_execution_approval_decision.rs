#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CommandExecutionApprovalDecisionVariant01 {
    #[serde(rename = "accept")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CommandExecutionApprovalDecisionVariant12 {
    #[serde(rename = "acceptForSession")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecutionApprovalDecisionVariant23AcceptWithExecpolicyAmendment4 {
    #[serde(rename = "execpolicy_amendment")]
    pub execpolicy_amendment: Box<crate::app_server::protocol::generated::v2::exec_policy_amendment::ExecPolicyAmendment>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecutionApprovalDecisionVariant23 {
    #[serde(rename = "acceptWithExecpolicyAmendment")]
    pub accept_with_execpolicy_amendment: CommandExecutionApprovalDecisionVariant23AcceptWithExecpolicyAmendment4,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecutionApprovalDecisionVariant35ApplyNetworkPolicyAmendment6 {
    #[serde(rename = "network_policy_amendment")]
    pub network_policy_amendment: Box<crate::app_server::protocol::generated::v2::network_policy_amendment::NetworkPolicyAmendment>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecutionApprovalDecisionVariant35 {
    #[serde(rename = "applyNetworkPolicyAmendment")]
    pub apply_network_policy_amendment: CommandExecutionApprovalDecisionVariant35ApplyNetworkPolicyAmendment6,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CommandExecutionApprovalDecisionVariant47 {
    #[serde(rename = "decline")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CommandExecutionApprovalDecisionVariant58 {
    #[serde(rename = "cancel")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum CommandExecutionApprovalDecision {
    Variant0(Box<CommandExecutionApprovalDecisionVariant01>),
    Variant1(Box<CommandExecutionApprovalDecisionVariant12>),
    Variant2(Box<CommandExecutionApprovalDecisionVariant23>),
    Variant3(Box<CommandExecutionApprovalDecisionVariant35>),
    Variant4(Box<CommandExecutionApprovalDecisionVariant47>),
    Variant5(Box<CommandExecutionApprovalDecisionVariant58>),
}
