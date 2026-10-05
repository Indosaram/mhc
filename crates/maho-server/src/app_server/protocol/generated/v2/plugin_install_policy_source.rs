#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginInstallPolicySource {
    #[serde(rename = "WORKSPACE_SETTING")]
    WORKSPACESETTING,
    #[serde(rename = "IMPLICIT_CANONICAL_APP")]
    IMPLICITCANONICALAPP,
}
