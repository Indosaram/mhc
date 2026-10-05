#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookErrorInfo {
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "message")]
    pub message: String,
}
