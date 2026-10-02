#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountApiKey1 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountChatgpt2 {
    #[serde(rename = "email", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub email: Option<String>,
    #[serde(rename = "planType")]
    pub plan_type: Box<crate::app_server::protocol::generated::plan_type::PlanType>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountAmazonBedrock3 {
    #[serde(rename = "usesCodexManagedCredentials")]
    pub uses_codex_managed_credentials: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum Account {
    #[serde(rename = "apiKey")]
    ApiKey(Box<AccountApiKey1>),
    #[serde(rename = "chatgpt")]
    Chatgpt(Box<AccountChatgpt2>),
    #[serde(rename = "amazonBedrock")]
    AmazonBedrock(Box<AccountAmazonBedrock3>),
}
