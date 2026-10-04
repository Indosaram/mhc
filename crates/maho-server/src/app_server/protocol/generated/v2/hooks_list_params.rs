#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HooksListParams {
    #[serde(rename = "cwds", default, skip_serializing_if = "Option::is_none")]
    pub cwds: Option<Vec<String>>,
}
