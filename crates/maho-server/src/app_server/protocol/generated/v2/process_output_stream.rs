#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ProcessOutputStream {
    #[serde(rename = "stdout")]
    Stdout,
    #[serde(rename = "stderr")]
    Stderr,
}
