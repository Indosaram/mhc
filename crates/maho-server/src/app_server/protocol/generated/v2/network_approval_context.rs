#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NetworkApprovalContext {
    #[serde(rename = "host")]
    pub host: String,
    #[serde(rename = "protocol")]
    pub protocol: Box<crate::app_server::protocol::generated::v2::network_approval_protocol::NetworkApprovalProtocol>,
}
