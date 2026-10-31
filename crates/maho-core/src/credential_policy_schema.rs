//! Port of senpi packages/coding-agent/src/core/credential-policy-schema.ts.
//!
//! Policy only: named slots REFERENCE env vars, command values or configuration references; key material
//! itself stays in auth.json or the environment, and unknown keys (for example a literal `apiKey`) are
//! rejected outright. Policy defaults are shared with the pool engine so schema and runtime cannot drift.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialSlotRef {
    pub env: Option<String>,
    /// senpi#3858: a command/template string or a configuration reference (`NonemptyConfigValueSchema`).
    pub value: Option<crate::config_reference_resolver::ConfigValue>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelsJsonCredentialPolicy {
    pub rotation: Option<bool>,
    pub affinity: Option<bool>,
    pub cooldown_base_ms: Option<f64>,
    pub cooldown_cap_ms: Option<f64>,
    pub slots: Option<IndexMap<String, CredentialSlotRef>>,
}

#[derive(Debug, Clone, Copy)]
pub struct CredentialPolicyDefaults {
    pub rotation: bool,
    pub affinity: bool,
    pub cooldown_base_ms: f64,
    pub cooldown_cap_ms: f64,
}

pub const CREDENTIAL_POLICY_DEFAULTS: CredentialPolicyDefaults = CredentialPolicyDefaults {
    rotation: true,
    affinity: true,
    cooldown_base_ms: maho_ai::auth::pool::failover::DEFAULT_SLOT_BLOCK_MS,
    cooldown_cap_ms: maho_ai::auth::pool::failover::MAX_SLOT_BLOCK_MS,
};
