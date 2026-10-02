#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteControlDisableParams {
    #[serde(rename = "ephemeral", default, skip_serializing_if = "Option::is_none")]
    pub ephemeral: Option<bool>,
}
