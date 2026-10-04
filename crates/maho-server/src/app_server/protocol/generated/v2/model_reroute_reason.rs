#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ModelRerouteReason {
    #[serde(rename = "highRiskCyberActivity")]
    Value,
}
