#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubagentMigration {
    #[serde(rename = "name")]
    pub name: String,
}
