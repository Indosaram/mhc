#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextElement {
    #[serde(rename = "byteRange")]
    pub byte_range: Box<crate::app_server::protocol::generated::v2::byte_range::ByteRange>,
    #[serde(rename = "placeholder", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub placeholder: Option<String>,
}
