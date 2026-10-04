#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginInterface {
    #[serde(rename = "displayName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub display_name: Option<String>,
    #[serde(rename = "shortDescription", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub short_description: Option<String>,
    #[serde(rename = "longDescription", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub long_description: Option<String>,
    #[serde(rename = "developerName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub developer_name: Option<String>,
    #[serde(rename = "category", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub category: Option<String>,
    #[serde(rename = "capabilities")]
    pub capabilities: Vec<String>,
    #[serde(rename = "websiteUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub website_url: Option<String>,
    #[serde(rename = "privacyPolicyUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub privacy_policy_url: Option<String>,
    #[serde(rename = "termsOfServiceUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub terms_of_service_url: Option<String>,
    #[serde(rename = "defaultPrompt")]
    pub default_prompt: Option<Vec<String>>,
    #[serde(rename = "brandColor", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub brand_color: Option<String>,
    #[serde(rename = "composerIcon", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub composer_icon: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "composerIconUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub composer_icon_url: Option<String>,
    #[serde(rename = "logo", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub logo: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "logoDark", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub logo_dark: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "logoUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub logo_url: Option<String>,
    #[serde(rename = "logoUrlDark", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub logo_url_dark: Option<String>,
    #[serde(rename = "screenshots")]
    pub screenshots: Vec<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "screenshotUrls")]
    pub screenshot_urls: Vec<String>,
}
