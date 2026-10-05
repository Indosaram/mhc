#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerOauthLoginResponse {
    #[serde(rename = "authorizationUrl")]
    pub authorization_url: String,
}
