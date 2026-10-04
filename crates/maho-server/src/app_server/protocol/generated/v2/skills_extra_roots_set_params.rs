#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkillsExtraRootsSetParams {
    #[serde(rename = "extraRoots")]
    pub extra_roots: Vec<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
}
