#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginShareTargetRole {
    #[serde(rename = "reader")]
    Reader,
    #[serde(rename = "editor")]
    Editor,
}
