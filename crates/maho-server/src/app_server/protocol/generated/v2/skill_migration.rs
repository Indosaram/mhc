#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillMigration {
    #[serde(rename = "name")]
    pub name: String,
}
