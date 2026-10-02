#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CancelLoginAccountParams {
    #[serde(rename = "loginId")]
    pub login_id: String,
}
