#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ResidencyRequirement {
    #[serde(rename = "us")]
    Value,
}
