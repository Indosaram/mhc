#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookOutputEntry {
    #[serde(rename = "kind")]
    pub kind: Box<crate::app_server::protocol::generated::v2::hook_output_entry_kind::HookOutputEntryKind>,
    #[serde(rename = "text")]
    pub text: String,
}
