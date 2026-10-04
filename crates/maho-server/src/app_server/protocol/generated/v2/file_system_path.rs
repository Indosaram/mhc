#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemPathPath1 {
    #[serde(rename = "path")]
    pub path: Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemPathGlobPattern2 {
    #[serde(rename = "pattern")]
    pub pattern: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemPathSpecial3 {
    #[serde(rename = "value")]
    pub value: Box<crate::app_server::protocol::generated::v2::file_system_special_path::FileSystemSpecialPath>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum FileSystemPath {
    #[serde(rename = "path")]
    Path(Box<FileSystemPathPath1>),
    #[serde(rename = "glob_pattern")]
    GlobPattern(Box<FileSystemPathGlobPattern2>),
    #[serde(rename = "special")]
    Special(Box<FileSystemPathSpecial3>),
}
