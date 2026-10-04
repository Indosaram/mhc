#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfiguredHookHandlerCommand1 {
    #[serde(rename = "command")]
    pub command: String,
    #[serde(rename = "commandWindows", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub command_windows: Option<String>,
    #[serde(rename = "timeoutSec", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub timeout_sec: Option<i64>,
    #[serde(rename = "async")]
    pub r#async: bool,
    #[serde(rename = "statusMessage", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub status_message: Option<String>,
    #[serde(rename = "additionalContextLimit", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub additional_context_limit: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfiguredHookHandlerPrompt2 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfiguredHookHandlerAgent3 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ConfiguredHookHandler {
    #[serde(rename = "command")]
    Command(Box<ConfiguredHookHandlerCommand1>),
    #[serde(rename = "prompt")]
    Prompt(Box<ConfiguredHookHandlerPrompt2>),
    #[serde(rename = "agent")]
    Agent(Box<ConfiguredHookHandlerAgent3>),
}
