#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpAuthStatus {
    #[serde(rename = "unsupported")]
    Unsupported,
    #[serde(rename = "notLoggedIn")]
    NotLoggedIn,
    #[serde(rename = "bearerToken")]
    BearerToken,
    #[serde(rename = "oAuth")]
    OAuth,
}
