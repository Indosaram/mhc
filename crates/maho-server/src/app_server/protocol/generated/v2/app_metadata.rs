#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppMetadata {
    #[serde(rename = "review", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub review: Option<Box<crate::app_server::protocol::generated::v2::app_review::AppReview>>,
    #[serde(rename = "categories", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub categories: Option<Vec<String>>,
    #[serde(rename = "subCategories", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub sub_categories: Option<Vec<String>>,
    #[serde(rename = "seoDescription", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub seo_description: Option<String>,
    #[serde(rename = "screenshots", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub screenshots: Option<Vec<Box<crate::app_server::protocol::generated::v2::app_screenshot::AppScreenshot>>>,
    #[serde(rename = "developer", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub developer: Option<String>,
    #[serde(rename = "version", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub version: Option<String>,
    #[serde(rename = "versionId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub version_id: Option<String>,
    #[serde(rename = "versionNotes", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub version_notes: Option<String>,
    #[serde(rename = "firstPartyType", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub first_party_type: Option<String>,
    #[serde(rename = "firstPartyRequiresInstall", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub first_party_requires_install: Option<bool>,
    #[serde(rename = "showInComposerWhenUnlinked", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub show_in_composer_when_unlinked: Option<bool>,
}
