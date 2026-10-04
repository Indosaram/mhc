#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NonSteerableTurnKind {
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "compact")]
    Compact,
}
