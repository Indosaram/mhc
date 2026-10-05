#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecTerminalSize {
    #[serde(rename = "rows")]
    pub rows: f64,
    #[serde(rename = "cols")]
    pub cols: f64,
}
