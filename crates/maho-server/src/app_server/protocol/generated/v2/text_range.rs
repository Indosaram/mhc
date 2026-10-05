#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextRange {
    #[serde(rename = "start")]
    pub start: Box<crate::app_server::protocol::generated::v2::text_position::TextPosition>,
    #[serde(rename = "end")]
    pub end: Box<crate::app_server::protocol::generated::v2::text_position::TextPosition>,
}
