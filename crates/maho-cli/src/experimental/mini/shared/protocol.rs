use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef { pub provider: String, pub model_id: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary { pub provider: String, pub model_id: String, pub name: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType { Oauth, ApiKey }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccount {
    pub id: String, pub name: String, pub auth_type: AuthType, pub configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>, pub interactive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method_name: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelsState { pub models: Vec<ModelSummary>, pub accounts: Vec<ProviderAccount>, pub refreshing: bool }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary { pub id: String, pub path: String, pub cwd: String, pub created_at: f64 }
#[derive(Clone, Copy)]
pub struct ServiceToken { pub name: &'static str }
pub const fn define_service(name: &'static str) -> ServiceToken { ServiceToken { name } }
pub const LANE: ServiceToken = define_service("lane");
pub const MODELS: ServiceToken = define_service("models");
pub const WORKER: ServiceToken = define_service("worker");
pub const SESSIONS: ServiceToken = define_service("sessions");
