#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetConversationSummaryParamsVariant01 {
    #[serde(rename = "rolloutPath")]
    pub rollout_path: String,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetConversationSummaryParamsVariant12 {
    #[serde(rename = "conversationId")]
    pub conversation_id: Box<crate::app_server::protocol::generated::thread_id::ThreadId>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum GetConversationSummaryParams {
    Variant0(Box<GetConversationSummaryParamsVariant01>),
    Variant1(Box<GetConversationSummaryParamsVariant12>),
}
