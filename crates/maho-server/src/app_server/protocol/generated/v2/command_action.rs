#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandActionRead1 {
    #[serde(rename = "command")]
    pub command: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "path")]
    pub path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandActionListFiles2 {
    #[serde(rename = "command")]
    pub command: String,
    #[serde(rename = "path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandActionSearch3 {
    #[serde(rename = "command")]
    pub command: String,
    #[serde(rename = "query", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub query: Option<String>,
    #[serde(rename = "path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandActionUnknown4 {
    #[serde(rename = "command")]
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum CommandAction {
    #[serde(rename = "read")]
    Read(Box<CommandActionRead1>),
    #[serde(rename = "listFiles")]
    ListFiles(Box<CommandActionListFiles2>),
    #[serde(rename = "search")]
    Search(Box<CommandActionSearch3>),
    #[serde(rename = "unknown")]
    Unknown(Box<CommandActionUnknown4>),
}
