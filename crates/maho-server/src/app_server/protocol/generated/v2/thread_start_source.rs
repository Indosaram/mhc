#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThreadStartSource {
    #[serde(rename = "startup")]
    Startup,
    #[serde(rename = "clear")]
    Clear,
}
