#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelAvailabilityNux {
    #[serde(rename = "message")]
    pub message: String,
}
