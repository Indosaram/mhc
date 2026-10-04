#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecTerminateParams {
    #[serde(rename = "processId")]
    pub process_id: String,
}
