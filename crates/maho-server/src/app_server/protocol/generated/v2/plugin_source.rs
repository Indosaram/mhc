#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginSourceLocal1 {
    #[serde(rename = "path")]
    pub path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginSourceGit2 {
    #[serde(rename = "url")]
    pub url: String,
    #[serde(rename = "path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub path: Option<String>,
    #[serde(rename = "refName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub ref_name: Option<String>,
    #[serde(rename = "sha", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub sha: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginSourceNpm3 {
    #[serde(rename = "package")]
    pub package: String,
    #[serde(rename = "version", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub version: Option<String>,
    #[serde(rename = "registry", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub registry: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginSourceRemote4 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum PluginSource {
    #[serde(rename = "local")]
    Local(Box<PluginSourceLocal1>),
    #[serde(rename = "git")]
    Git(Box<PluginSourceGit2>),
    #[serde(rename = "npm")]
    Npm(Box<PluginSourceNpm3>),
    #[serde(rename = "remote")]
    Remote(Box<PluginSourceRemote4>),
}
