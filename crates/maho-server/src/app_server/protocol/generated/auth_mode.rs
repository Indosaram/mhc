#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AuthMode {
    #[serde(rename = "apikey")]
    Apikey,
    #[serde(rename = "chatgpt")]
    Chatgpt,
    #[serde(rename = "chatgptAuthTokens")]
    ChatgptAuthTokens,
    #[serde(rename = "headers")]
    Headers,
    #[serde(rename = "agentIdentity")]
    AgentIdentity,
    #[serde(rename = "personalAccessToken")]
    PersonalAccessToken,
    #[serde(rename = "bedrockApiKey")]
    BedrockApiKey,
}
