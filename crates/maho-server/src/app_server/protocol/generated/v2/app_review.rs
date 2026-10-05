#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppReview {
    #[serde(rename = "status")]
    pub status: String,
}
