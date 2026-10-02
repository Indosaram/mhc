#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RemoteControlConnectionStatus {
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "connecting")]
    Connecting,
    #[serde(rename = "connected")]
    Connected,
    #[serde(rename = "errored")]
    Errored,
}
