#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountParamsApiKey1 {
    #[serde(rename = "apiKey")]
    pub api_key: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountParamsChatgpt2 {
    #[serde(rename = "codexStreamlinedLogin", default, skip_serializing_if = "Option::is_none")]
    pub codex_streamlined_login: Option<bool>,
    #[serde(rename = "useHostedLoginSuccessPage", default, skip_serializing_if = "Option::is_none")]
    pub use_hosted_login_success_page: Option<bool>,
    #[serde(rename = "appBrand", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub app_brand: Option<Option<Box<crate::app_server::protocol::generated::v2::login_app_brand::LoginAppBrand>>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountParamsChatgptDeviceCode3 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountParamsChatgptAuthTokens4 {
    #[serde(rename = "accessToken")]
    pub access_token: String,
    #[serde(rename = "chatgptAccountId")]
    pub chatgpt_account_id: String,
    #[serde(rename = "chatgptPlanType", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub chatgpt_plan_type: Option<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LoginAccountParamsAmazonBedrock5 {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    #[serde(rename = "region")]
    pub region: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum LoginAccountParams {
    #[serde(rename = "apiKey")]
    ApiKey(Box<LoginAccountParamsApiKey1>),
    #[serde(rename = "chatgpt")]
    Chatgpt(Box<LoginAccountParamsChatgpt2>),
    #[serde(rename = "chatgptDeviceCode")]
    ChatgptDeviceCode(Box<LoginAccountParamsChatgptDeviceCode3>),
    #[serde(rename = "chatgptAuthTokens")]
    ChatgptAuthTokens(Box<LoginAccountParamsChatgptAuthTokens4>),
    #[serde(rename = "amazonBedrock")]
    AmazonBedrock(Box<LoginAccountParamsAmazonBedrock5>),
}
