pub type ToolRequestUserInputResponseAnswers1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::v2::tool_request_user_input_answer::ToolRequestUserInputAnswer>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolRequestUserInputResponse {
    #[serde(rename = "answers")]
    pub answers: ToolRequestUserInputResponseAnswers1,
}
