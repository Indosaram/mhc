#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WriteStatus {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "okOverridden")]
    OkOverridden,
}
