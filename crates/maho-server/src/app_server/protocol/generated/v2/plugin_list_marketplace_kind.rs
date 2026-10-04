#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginListMarketplaceKind {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "vertical")]
    Vertical,
    #[serde(rename = "workspace-directory")]
    WorkspaceDirectory,
    #[serde(rename = "shared-with-me")]
    SharedWithMe,
    #[serde(rename = "created-by-me-remote")]
    CreatedByMeRemote,
}
