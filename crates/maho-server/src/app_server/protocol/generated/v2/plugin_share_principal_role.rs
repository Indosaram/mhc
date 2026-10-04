#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginSharePrincipalRole {
    #[serde(rename = "reader")]
    Reader,
    #[serde(rename = "editor")]
    Editor,
    #[serde(rename = "owner")]
    Owner,
}
