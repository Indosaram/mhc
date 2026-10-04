#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolCallOutputContentItemInputText1 {
    #[serde(rename = "text")]
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolCallOutputContentItemInputImage2 {
    #[serde(rename = "imageUrl")]
    pub image_url: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolCallOutputContentItemInputAudio3 {
    #[serde(rename = "audioUrl")]
    pub audio_url: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum DynamicToolCallOutputContentItem {
    #[serde(rename = "inputText")]
    InputText(Box<DynamicToolCallOutputContentItemInputText1>),
    #[serde(rename = "inputImage")]
    InputImage(Box<DynamicToolCallOutputContentItemInputImage2>),
    #[serde(rename = "inputAudio")]
    InputAudio(Box<DynamicToolCallOutputContentItemInputAudio3>),
}
