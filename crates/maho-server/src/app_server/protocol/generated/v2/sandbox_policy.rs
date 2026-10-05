#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SandboxPolicyDangerFullAccess1 {
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SandboxPolicyReadOnly2 {
    #[serde(rename = "networkAccess")]
    pub network_access: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SandboxPolicyExternalSandbox3 {
    #[serde(rename = "networkAccess")]
    pub network_access: Box<crate::app_server::protocol::generated::v2::network_access::NetworkAccess>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SandboxPolicyWorkspaceWrite4 {
    #[serde(rename = "writableRoots")]
    pub writable_roots: Vec<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "networkAccess")]
    pub network_access: bool,
    #[serde(rename = "excludeTmpdirEnvVar")]
    pub exclude_tmpdir_env_var: bool,
    #[serde(rename = "excludeSlashTmp")]
    pub exclude_slash_tmp: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum SandboxPolicy {
    #[serde(rename = "dangerFullAccess")]
    DangerFullAccess(Box<SandboxPolicyDangerFullAccess1>),
    #[serde(rename = "readOnly")]
    ReadOnly(Box<SandboxPolicyReadOnly2>),
    #[serde(rename = "externalSandbox")]
    ExternalSandbox(Box<SandboxPolicyExternalSandbox3>),
    #[serde(rename = "workspaceWrite")]
    WorkspaceWrite(Box<SandboxPolicyWorkspaceWrite4>),
}
