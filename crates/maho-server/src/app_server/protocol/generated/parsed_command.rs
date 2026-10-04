#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParsedCommandRead1 {
    #[serde(rename = "cmd")]
    pub cmd: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "path")]
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParsedCommandListFiles2 {
    #[serde(rename = "cmd")]
    pub cmd: String,
    #[serde(rename = "path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParsedCommandSearch3 {
    #[serde(rename = "cmd")]
    pub cmd: String,
    #[serde(rename = "query", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub query: Option<String>,
    #[serde(rename = "path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParsedCommandUnknown4 {
    #[serde(rename = "cmd")]
    pub cmd: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum ParsedCommand {
    #[serde(rename = "read")]
    Read(Box<ParsedCommandRead1>),
    #[serde(rename = "list_files")]
    ListFiles(Box<ParsedCommandListFiles2>),
    #[serde(rename = "search")]
    Search(Box<ParsedCommandSearch3>),
    #[serde(rename = "unknown")]
    Unknown(Box<ParsedCommandUnknown4>),
}
