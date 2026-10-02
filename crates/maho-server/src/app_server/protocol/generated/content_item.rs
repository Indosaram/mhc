#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContentItemInputText1 {
    #[serde(rename = "text")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContentItemInputImage2 {
    #[serde(rename = "image_url")]
    pub image_url: String,
    #[serde(rename = "detail", default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Box<crate::app_server::protocol::generated::image_detail::ImageDetail>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContentItemInputAudio3 {
    #[serde(rename = "audio_url")]
    pub audio_url: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContentItemOutputText4 {
    #[serde(rename = "text")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ContentItem {
    #[serde(rename = "input_text")]
    InputText(Box<ContentItemInputText1>),
    #[serde(rename = "input_image")]
    InputImage(Box<ContentItemInputImage2>),
    #[serde(rename = "input_audio")]
    InputAudio(Box<ContentItemInputAudio3>),
    #[serde(rename = "output_text")]
    OutputText(Box<ContentItemOutputText4>),
}
