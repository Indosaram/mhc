#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserInputText1 {
    #[serde(rename = "text")]
    pub text: String,
    #[serde(rename = "text_elements")]
    pub text_elements: Vec<Box<crate::app_server::protocol::generated::v2::text_element::TextElement>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserInputImage2 {
    #[serde(rename = "detail", default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Box<crate::app_server::protocol::generated::image_detail::ImageDetail>>,
    #[serde(rename = "url")]
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserInputLocalImage3 {
    #[serde(rename = "detail", default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Box<crate::app_server::protocol::generated::image_detail::ImageDetail>>,
    #[serde(rename = "path")]
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserInputAudio4 {
    #[serde(rename = "url")]
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserInputLocalAudio5 {
    #[serde(rename = "path")]
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserInputSkill6 {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "path")]
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserInputMention7 {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "path")]
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum UserInput {
    #[serde(rename = "text")]
    Text(Box<UserInputText1>),
    #[serde(rename = "image")]
    Image(Box<UserInputImage2>),
    #[serde(rename = "localImage")]
    LocalImage(Box<UserInputLocalImage3>),
    #[serde(rename = "audio")]
    Audio(Box<UserInputAudio4>),
    #[serde(rename = "localAudio")]
    LocalAudio(Box<UserInputLocalAudio5>),
    #[serde(rename = "skill")]
    Skill(Box<UserInputSkill6>),
    #[serde(rename = "mention")]
    Mention(Box<UserInputMention7>),
}
