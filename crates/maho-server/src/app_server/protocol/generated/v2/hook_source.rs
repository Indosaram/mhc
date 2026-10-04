#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HookSource {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "project")]
    Project,
    #[serde(rename = "mdm")]
    Mdm,
    #[serde(rename = "sessionFlags")]
    SessionFlags,
    #[serde(rename = "plugin")]
    Plugin,
    #[serde(rename = "cloudRequirements")]
    CloudRequirements,
    #[serde(rename = "cloudManagedConfig")]
    CloudManagedConfig,
    #[serde(rename = "legacyManagedConfigFile")]
    LegacyManagedConfigFile,
    #[serde(rename = "legacyManagedConfigMdm")]
    LegacyManagedConfigMdm,
    #[serde(rename = "unknown")]
    Unknown,
}
