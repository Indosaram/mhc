#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecOutputDeltaNotification {
    #[serde(rename = "processId")]
    pub process_id: String,
    #[serde(rename = "stream")]
    pub stream: Box<crate::app_server::protocol::generated::v2::command_exec_output_stream::CommandExecOutputStream>,
    #[serde(rename = "deltaBase64")]
    pub delta_base64: String,
    #[serde(rename = "capReached")]
    pub cap_reached: bool,
}
