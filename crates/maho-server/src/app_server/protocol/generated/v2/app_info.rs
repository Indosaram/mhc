pub type AppInfoIconAssets1 = std::collections::BTreeMap<String, String>;

pub type AppInfoIconDarkAssets2 = std::collections::BTreeMap<String, String>;

pub type AppInfoLabels3 = std::collections::BTreeMap<String, String>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppInfo {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
    #[serde(rename = "logoUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub logo_url: Option<String>,
    #[serde(rename = "logoUrlDark", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub logo_url_dark: Option<String>,
    #[serde(rename = "iconAssets", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub icon_assets: Option<AppInfoIconAssets1>,
    #[serde(rename = "iconDarkAssets", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub icon_dark_assets: Option<AppInfoIconDarkAssets2>,
    #[serde(rename = "distributionChannel", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub distribution_channel: Option<String>,
    #[serde(rename = "branding", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub branding: Option<Box<crate::app_server::protocol::generated::v2::app_branding::AppBranding>>,
    #[serde(rename = "appMetadata", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub app_metadata: Option<Box<crate::app_server::protocol::generated::v2::app_metadata::AppMetadata>>,
    #[serde(rename = "labels", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub labels: Option<AppInfoLabels3>,
    #[serde(rename = "installUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub install_url: Option<String>,
    #[serde(rename = "isAccessible")]
    pub is_accessible: bool,
    #[serde(rename = "isEnabled")]
    pub is_enabled: bool,
    #[serde(rename = "pluginDisplayNames")]
    pub plugin_display_names: Vec<String>,
}
