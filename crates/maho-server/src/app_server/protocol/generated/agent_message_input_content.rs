#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentMessageInputContentInputText1 {
    #[serde(rename = "text")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentMessageInputContentEncryptedContent2 {
    #[serde(rename = "encrypted_content")]
    pub encrypted_content: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum AgentMessageInputContent {
    #[serde(rename = "input_text")]
    InputText(Box<AgentMessageInputContentInputText1>),
    #[serde(rename = "encrypted_content")]
    EncryptedContent(Box<AgentMessageInputContentEncryptedContent2>),
}
