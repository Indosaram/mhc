#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AppTemplateUnavailableReason {
    #[serde(rename = "NOT_CONFIGURED_FOR_WORKSPACE")]
    NOTCONFIGUREDFORWORKSPACE,
    #[serde(rename = "NO_ACTIVE_WORKSPACE")]
    NOACTIVEWORKSPACE,
}
