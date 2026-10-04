#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NetworkPolicyRuleAction {
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "deny")]
    Deny,
}
