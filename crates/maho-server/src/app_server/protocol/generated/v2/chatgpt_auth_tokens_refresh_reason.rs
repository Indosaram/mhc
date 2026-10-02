#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ChatgptAuthTokensRefreshReason {
    #[serde(rename = "unauthorized")]
    Value,
}
