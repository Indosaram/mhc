#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookMigration {
    #[serde(rename = "name")]
    pub name: String,
}
