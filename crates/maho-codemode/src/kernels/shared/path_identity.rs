pub const PATH_IDENTITY_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalPath {
    pub path: String,
    pub fold_case: bool,
}
