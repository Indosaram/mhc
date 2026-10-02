#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CollaborationMode {
    #[serde(rename = "mode")]
    pub mode: Box<crate::app_server::protocol::generated::mode_kind::ModeKind>,
    #[serde(rename = "settings")]
    pub settings: Box<crate::app_server::protocol::generated::settings::Settings>,
}
