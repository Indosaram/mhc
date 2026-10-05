#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AutoReviewDecisionSource {
    #[serde(rename = "agent")]
    Value,
}
