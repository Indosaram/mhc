#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FunctionCallOutputContentItemInputText1 {
    #[serde(rename = "text")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FunctionCallOutputContentItemInputImage2 {
    #[serde(rename = "image_url")]
    pub image_url: String,
    #[serde(rename = "detail", default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Box<crate::app_server::protocol::generated::image_detail::ImageDetail>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FunctionCallOutputContentItemInputAudio3 {
    #[serde(rename = "audio_url")]
    pub audio_url: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FunctionCallOutputContentItemEncryptedContent4 {
    #[serde(rename = "encrypted_content")]
    pub encrypted_content: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum FunctionCallOutputContentItem {
    #[serde(rename = "input_text")]
    InputText(Box<FunctionCallOutputContentItemInputText1>),
    #[serde(rename = "input_image")]
    InputImage(Box<FunctionCallOutputContentItemInputImage2>),
    #[serde(rename = "input_audio")]
    InputAudio(Box<FunctionCallOutputContentItemInputAudio3>),
    #[serde(rename = "encrypted_content")]
    EncryptedContent(Box<FunctionCallOutputContentItemEncryptedContent4>),
}
