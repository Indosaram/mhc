#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProcessOutputDeltaNotification {
    #[serde(rename = "processHandle")]
    pub process_handle: String,
    #[serde(rename = "stream")]
    pub stream: Box<crate::app_server::protocol::generated::v2::process_output_stream::ProcessOutputStream>,
    #[serde(rename = "deltaBase64")]
    pub delta_base64: String,
    #[serde(rename = "capReached")]
    pub cap_reached: bool,
}
