#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoalClearResponse {
    #[serde(rename = "cleared")]
    pub cleared: bool,
}
