pub type CommandExecParamsEnv1 = std::collections::BTreeMap<String, Option<String>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecParams {
    #[serde(rename = "command")]
    pub command: Vec<String>,
    #[serde(rename = "processId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub process_id: Option<Option<String>>,
    #[serde(rename = "tty", default, skip_serializing_if = "Option::is_none")]
    pub tty: Option<bool>,
    #[serde(rename = "streamStdin", default, skip_serializing_if = "Option::is_none")]
    pub stream_stdin: Option<bool>,
    #[serde(rename = "streamStdoutStderr", default, skip_serializing_if = "Option::is_none")]
    pub stream_stdout_stderr: Option<bool>,
    #[serde(rename = "outputBytesCap", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub output_bytes_cap: Option<Option<f64>>,
    #[serde(rename = "disableOutputCap", default, skip_serializing_if = "Option::is_none")]
    pub disable_output_cap: Option<bool>,
    #[serde(rename = "disableTimeout", default, skip_serializing_if = "Option::is_none")]
    pub disable_timeout: Option<bool>,
    #[serde(rename = "timeoutMs", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub timeout_ms: Option<Option<f64>>,
    #[serde(rename = "cwd", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub cwd: Option<Option<String>>,
    #[serde(rename = "env", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub env: Option<Option<CommandExecParamsEnv1>>,
    #[serde(rename = "size", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub size: Option<Option<Box<crate::app_server::protocol::generated::v2::command_exec_terminal_size::CommandExecTerminalSize>>>,
    #[serde(rename = "sandboxPolicy", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub sandbox_policy: Option<Option<Box<crate::app_server::protocol::generated::v2::sandbox_policy::SandboxPolicy>>>,
}
