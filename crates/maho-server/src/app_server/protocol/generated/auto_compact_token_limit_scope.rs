#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AutoCompactTokenLimitScope {
    #[serde(rename = "total")]
    Total,
    #[serde(rename = "body_after_prefix")]
    BodyAfterPrefix,
}
