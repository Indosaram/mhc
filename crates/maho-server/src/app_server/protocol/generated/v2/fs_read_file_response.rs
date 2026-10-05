#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsReadFileResponse {
    #[serde(rename = "dataBase64")]
    pub data_base64: String,
}
