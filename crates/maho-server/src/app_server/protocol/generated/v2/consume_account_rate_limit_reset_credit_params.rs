#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConsumeAccountRateLimitResetCreditParams {
    #[serde(rename = "idempotencyKey")]
    pub idempotency_key: String,
    #[serde(rename = "creditId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub credit_id: Option<Option<String>>,
}
