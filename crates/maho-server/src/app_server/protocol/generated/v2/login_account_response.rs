#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountResponseApiKey1 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountResponseChatgpt2 {
    #[serde(rename = "loginId")]
    pub login_id: String,
    #[serde(rename = "authUrl")]
    pub auth_url: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountResponseChatgptDeviceCode3 {
    #[serde(rename = "loginId")]
    pub login_id: String,
    #[serde(rename = "verificationUrl")]
    pub verification_url: String,
    #[serde(rename = "userCode")]
    pub user_code: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountResponseChatgptAuthTokens4 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountResponseAmazonBedrock5 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum LoginAccountResponse {
    #[serde(rename = "apiKey")]
    ApiKey(Box<LoginAccountResponseApiKey1>),
    #[serde(rename = "chatgpt")]
    Chatgpt(Box<LoginAccountResponseChatgpt2>),
    #[serde(rename = "chatgptDeviceCode")]
    ChatgptDeviceCode(Box<LoginAccountResponseChatgptDeviceCode3>),
    #[serde(rename = "chatgptAuthTokens")]
    ChatgptAuthTokens(Box<LoginAccountResponseChatgptAuthTokens4>),
    #[serde(rename = "amazonBedrock")]
    AmazonBedrock(Box<LoginAccountResponseAmazonBedrock5>),
}
