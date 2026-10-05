#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReviewDecisionVariant01 {
    #[serde(rename = "approved")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewDecisionVariant12ApprovedExecpolicyAmendment3 {
    #[serde(rename = "proposed_execpolicy_amendment")]
    pub proposed_execpolicy_amendment: Box<crate::app_server::protocol::generated::exec_policy_amendment::ExecPolicyAmendment>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewDecisionVariant12 {
    #[serde(rename = "approved_execpolicy_amendment")]
    pub approved_execpolicy_amendment: ReviewDecisionVariant12ApprovedExecpolicyAmendment3,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReviewDecisionVariant24 {
    #[serde(rename = "approved_for_session")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewDecisionVariant35NetworkPolicyAmendment6 {
    #[serde(rename = "network_policy_amendment")]
    pub network_policy_amendment: Box<crate::app_server::protocol::generated::network_policy_amendment::NetworkPolicyAmendment>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewDecisionVariant35 {
    #[serde(rename = "network_policy_amendment")]
    pub network_policy_amendment: ReviewDecisionVariant35NetworkPolicyAmendment6,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewDecisionVariant47Denied8 {
    #[serde(rename = "rejection")]
    pub rejection: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewDecisionVariant47 {
    #[serde(rename = "denied")]
    pub denied: ReviewDecisionVariant47Denied8,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReviewDecisionVariant59 {
    #[serde(rename = "timed_out")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReviewDecisionVariant610 {
    #[serde(rename = "abort")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum ReviewDecision {
    Variant0(Box<ReviewDecisionVariant01>),
    Variant1(Box<ReviewDecisionVariant12>),
    Variant2(Box<ReviewDecisionVariant24>),
    Variant3(Box<ReviewDecisionVariant35>),
    Variant4(Box<ReviewDecisionVariant47>),
    Variant5(Box<ReviewDecisionVariant59>),
    Variant6(Box<ReviewDecisionVariant610>),
}
