#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NetworkDomainPermission {
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "deny")]
    Deny,
}
