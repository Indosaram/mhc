#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HookHandlerType {
    #[serde(rename = "command")]
    Command,
    #[serde(rename = "prompt")]
    Prompt,
    #[serde(rename = "agent")]
    Agent,
}
