#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HookScope {
    #[serde(rename = "thread")]
    Thread,
    #[serde(rename = "turn")]
    Turn,
}
