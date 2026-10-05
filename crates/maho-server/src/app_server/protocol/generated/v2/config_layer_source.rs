#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerSourceMdm1 {
    #[serde(rename = "domain")]
    pub domain: String,
    #[serde(rename = "key")]
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerSourceSystem2 {
    #[serde(rename = "file")]
    pub file: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerSourceEnterpriseManaged3 {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "name")]
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerSourceUser4 {
    #[serde(rename = "file")]
    pub file: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "profile", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub profile: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerSourceProject5 {
    #[serde(rename = "dotCodexFolder")]
    pub dot_codex_folder: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerSourceSessionFlags6 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerSourceLegacyManagedConfigTomlFromFile7 {
    #[serde(rename = "file")]
    pub file: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigLayerSourceLegacyManagedConfigTomlFromMdm8 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ConfigLayerSource {
    #[serde(rename = "mdm")]
    Mdm(Box<ConfigLayerSourceMdm1>),
    #[serde(rename = "system")]
    System(Box<ConfigLayerSourceSystem2>),
    #[serde(rename = "enterpriseManaged")]
    EnterpriseManaged(Box<ConfigLayerSourceEnterpriseManaged3>),
    #[serde(rename = "user")]
    User(Box<ConfigLayerSourceUser4>),
    #[serde(rename = "project")]
    Project(Box<ConfigLayerSourceProject5>),
    #[serde(rename = "sessionFlags")]
    SessionFlags(Box<ConfigLayerSourceSessionFlags6>),
    #[serde(rename = "legacyManagedConfigTomlFromFile")]
    LegacyManagedConfigTomlFromFile(Box<ConfigLayerSourceLegacyManagedConfigTomlFromFile7>),
    #[serde(rename = "legacyManagedConfigTomlFromMdm")]
    LegacyManagedConfigTomlFromMdm(Box<ConfigLayerSourceLegacyManagedConfigTomlFromMdm8>),
}
