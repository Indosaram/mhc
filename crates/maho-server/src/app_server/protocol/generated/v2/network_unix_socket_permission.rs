#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NetworkUnixSocketPermission {
    #[serde(rename = "allow")]
    Allow,
    #[serde(rename = "deny")]
    Deny,
}
