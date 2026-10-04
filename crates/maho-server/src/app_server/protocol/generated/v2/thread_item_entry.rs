#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadItemEntry {
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "item")]
    pub item: Box<crate::app_server::protocol::generated::v2::thread_item::ThreadItem>,
}
