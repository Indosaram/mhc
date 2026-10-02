#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnStartResponse {
    #[serde(rename = "turn")]
    pub turn: Box<crate::app_server::protocol::generated::v2::turn::Turn>,
}
