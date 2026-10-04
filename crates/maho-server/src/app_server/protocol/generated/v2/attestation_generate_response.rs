#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AttestationGenerateResponse {
    #[serde(rename = "token")]
    pub token: String,
}
