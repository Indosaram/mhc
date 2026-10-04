#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SandboxWorkspaceWrite {
    #[serde(rename = "writable_roots")]
    pub writable_roots: Vec<String>,
    #[serde(rename = "network_access")]
    pub network_access: bool,
    #[serde(rename = "exclude_tmpdir_env_var")]
    pub exclude_tmpdir_env_var: bool,
    #[serde(rename = "exclude_slash_tmp")]
    pub exclude_slash_tmp: bool,
}
