use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OpenCodeBinaryType {
    #[default]
    Opencode,
    OpencodeDesktop,
}

#[derive(Debug, Clone)]
pub struct OpenCodeConfigDirOptions {
    pub binary: OpenCodeBinaryType,
    pub version: Option<String>,
    pub check_existing: bool,
}

impl Default for OpenCodeConfigDirOptions {
    fn default() -> Self {
        Self {
            binary: OpenCodeBinaryType::Opencode,
            version: None,
            check_existing: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenCodeConfigPaths {
    pub config_dir: PathBuf,
    pub config_json: PathBuf,
    pub config_jsonc: PathBuf,
    pub package_json: PathBuf,
    pub omo_config: PathBuf,
}
