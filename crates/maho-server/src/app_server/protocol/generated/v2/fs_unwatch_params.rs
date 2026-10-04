#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsUnwatchParams {
    #[serde(rename = "watchId")]
    pub watch_id: String,
}
