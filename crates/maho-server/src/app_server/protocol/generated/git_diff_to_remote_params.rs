#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GitDiffToRemoteParams {
    #[serde(rename = "cwd")]
    pub cwd: String,
}
