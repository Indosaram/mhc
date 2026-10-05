#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelsRequirements {
    #[serde(rename = "newThread", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub new_thread: Option<Box<crate::app_server::protocol::generated::v2::new_thread_model_defaults::NewThreadModelDefaults>>,
}
