#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PlanType {
    #[serde(rename = "free")]
    Free,
    #[serde(rename = "go")]
    Go,
    #[serde(rename = "plus")]
    Plus,
    #[serde(rename = "pro")]
    Pro,
    #[serde(rename = "prolite")]
    Prolite,
    #[serde(rename = "team")]
    Team,
    #[serde(rename = "self_serve_business_usage_based")]
    SelfServeBusinessUsageBased,
    #[serde(rename = "business")]
    Business,
    #[serde(rename = "enterprise_cbp_usage_based")]
    EnterpriseCbpUsageBased,
    #[serde(rename = "enterprise")]
    Enterprise,
    #[serde(rename = "edu")]
    Edu,
    #[serde(rename = "unknown")]
    Unknown,
}
