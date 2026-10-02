#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PermissionGrantScope {
    #[serde(rename = "turn")]
    Turn,
    #[serde(rename = "session")]
    Session,
}
