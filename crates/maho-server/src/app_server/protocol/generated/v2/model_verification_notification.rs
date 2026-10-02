#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelVerificationNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "verifications")]
    pub verifications: Vec<Box<crate::app_server::protocol::generated::v2::model_verification::ModelVerification>>,
}
