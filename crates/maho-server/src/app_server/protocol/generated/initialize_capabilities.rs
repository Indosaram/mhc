#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InitializeCapabilities {
    #[serde(rename = "experimentalApi")]
    pub experimental_api: bool,
    #[serde(rename = "requestAttestation")]
    pub request_attestation: bool,
    #[serde(rename = "mcpServerOpenaiFormElicitation", default, skip_serializing_if = "Option::is_none")]
    pub mcp_server_openai_form_elicitation: Option<bool>,
    #[serde(rename = "optOutNotificationMethods", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub opt_out_notification_methods: Option<Option<Vec<String>>>,
}
