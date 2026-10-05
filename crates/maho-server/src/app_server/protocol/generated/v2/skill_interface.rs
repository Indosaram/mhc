#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillInterface {
    #[serde(rename = "displayName", default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(rename = "shortDescription", default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    #[serde(rename = "iconSmall", default, skip_serializing_if = "Option::is_none")]
    pub icon_small: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "iconLarge", default, skip_serializing_if = "Option::is_none")]
    pub icon_large: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "brandColor", default, skip_serializing_if = "Option::is_none")]
    pub brand_color: Option<String>,
    #[serde(rename = "defaultPrompt", default, skip_serializing_if = "Option::is_none")]
    pub default_prompt: Option<String>,
}
