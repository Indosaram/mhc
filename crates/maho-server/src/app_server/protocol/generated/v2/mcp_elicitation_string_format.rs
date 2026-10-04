#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpElicitationStringFormat {
    #[serde(rename = "email")]
    Email,
    #[serde(rename = "uri")]
    Uri,
    #[serde(rename = "date")]
    Date,
    #[serde(rename = "date-time")]
    DateTime,
}
