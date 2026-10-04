#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LocalShellActionType1 {
    #[serde(rename = "exec")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LocalShellAction {
    #[serde(rename = "type")]
    pub r#type: LocalShellActionType1,
    #[serde(flatten)]
    pub details: Box<crate::app_server::protocol::generated::local_shell_exec_action::LocalShellExecAction>,
}
