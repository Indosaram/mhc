#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnSteerResponse {
    #[serde(rename = "turnId")]
    pub turn_id: String,
}
