#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandMigration {
    #[serde(rename = "name")]
    pub name: String,
}
