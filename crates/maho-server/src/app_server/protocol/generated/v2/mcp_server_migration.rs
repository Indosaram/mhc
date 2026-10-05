#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerMigration {
    #[serde(rename = "name")]
    pub name: String,
}
