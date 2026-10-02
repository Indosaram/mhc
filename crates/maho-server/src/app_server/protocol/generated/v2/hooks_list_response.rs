#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HooksListResponse {
    #[serde(rename = "data")]
    pub data: Vec<Box<crate::app_server::protocol::generated::v2::hooks_list_entry::HooksListEntry>>,
}
