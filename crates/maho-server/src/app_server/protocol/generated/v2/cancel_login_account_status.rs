#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CancelLoginAccountStatus {
    #[serde(rename = "canceled")]
    Canceled,
    #[serde(rename = "notFound")]
    NotFound,
}
