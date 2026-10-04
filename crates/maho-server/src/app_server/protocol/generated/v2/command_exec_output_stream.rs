#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CommandExecOutputStream {
    #[serde(rename = "stdout")]
    Stdout,
    #[serde(rename = "stderr")]
    Stderr,
}
