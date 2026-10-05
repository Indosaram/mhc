#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NetworkPolicyAmendment {
    #[serde(rename = "host")]
    pub host: String,
    #[serde(rename = "action")]
    pub action: Box<crate::app_server::protocol::generated::v2::network_policy_rule_action::NetworkPolicyRuleAction>,
}
