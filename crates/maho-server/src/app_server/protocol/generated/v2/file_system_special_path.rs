#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemSpecialPathRoot1 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemSpecialPathMinimal2 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemSpecialPathProjectRoots3 {
    #[serde(rename = "subpath", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub subpath: Option<Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemSpecialPathTmpdir4 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemSpecialPathSlashTmp5 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemSpecialPathUnknown6 {
    #[serde(rename = "path")]
    pub path: String,
    #[serde(rename = "subpath", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub subpath: Option<Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum FileSystemSpecialPath {
    #[serde(rename = "root")]
    Root(Box<FileSystemSpecialPathRoot1>),
    #[serde(rename = "minimal")]
    Minimal(Box<FileSystemSpecialPathMinimal2>),
    #[serde(rename = "project_roots")]
    ProjectRoots(Box<FileSystemSpecialPathProjectRoots3>),
    #[serde(rename = "tmpdir")]
    Tmpdir(Box<FileSystemSpecialPathTmpdir4>),
    #[serde(rename = "slash_tmp")]
    SlashTmp(Box<FileSystemSpecialPathSlashTmp5>),
    #[serde(rename = "unknown")]
    Unknown(Box<FileSystemSpecialPathUnknown6>),
}
