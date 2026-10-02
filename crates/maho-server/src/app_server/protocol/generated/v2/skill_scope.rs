#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SkillScope {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "repo")]
    Repo,
    #[serde(rename = "system")]
    System,
    #[serde(rename = "admin")]
    Admin,
}
