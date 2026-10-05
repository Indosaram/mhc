#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecResizeParams {
    #[serde(rename = "processId")]
    pub process_id: String,
    #[serde(rename = "size")]
    pub size: Box<crate::app_server::protocol::generated::v2::command_exec_terminal_size::CommandExecTerminalSize>,
}
