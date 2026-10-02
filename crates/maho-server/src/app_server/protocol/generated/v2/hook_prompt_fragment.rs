#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HookPromptFragment {
    #[serde(rename = "text")]
    pub text: String,
    #[serde(rename = "hookRunId")]
    pub hook_run_id: String,
}
