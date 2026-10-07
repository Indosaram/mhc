//! Port of omo-senpi `components/model-profile/credential-policy.ts` at omo `455dee62`: whether the
//! engine may rotate a provider's stored credential accounts on the first turn.
//!
//! senpi's `ModelRuntime.couldRotateCredentials` never rotates when a runtime API key is set for the
//! provider (`getProviderAuthStatus` reports `source: "runtime"`) or when `models.json` sets
//! `providers.<id>.credentials.rotation: false`; the request then resolves the flat (default)
//! credential only. The rotation policy lives only in `models.json` and the runtime exposes no
//! accessor for it, so it is read from the same file the engine loads: `<agentDir>/models.json`
//! (JSONC, as senpi parses it). An unreadable or absent file is the engine's default: rotation on.
//!
//! Native: the policy reads the registry's own `get_provider_auth_status` port (exact shared
//! signature in `.omo/authoring/model-profile.md` section 2) instead of an injected closure, so
//! production and the registered-hook tests exercise the same real accessor. The port's return type
//! is the SHARED ext-api type (this crate declares no auth-status type and must not: a
//! component-local type cannot be the trait's return type without an ext-api -> component cycle);
//! this module only reads `.source`, so it compiles against whichever shared type the host lands.
//! A default status (`source: None`) keeps rotation on, which is upstream's absent-accessor branch
//! (`if (typeof registry.getProviderAuthStatus !== "function") return true`); that fallback does NOT
//! by itself prove the runtime-key case - the host must report `source: "runtime"` for a runtime API
//! key.

use std::collections::BTreeSet;
use std::path::Path;

use maho_ext_api::{JsonValue, ModelRegistry};

/// Upstream `credentialRotationPolicy(registry, agentDir)`.
pub struct CredentialRotationPolicy<'a> {
    disabled: BTreeSet<String>,
    registry: &'a dyn ModelRegistry,
}

impl CredentialRotationPolicy<'_> {
    pub fn may_rotate(&self, provider: &str) -> bool {
        if self.disabled.contains(provider) {
            return false;
        }
        // senpi `getProviderAuthStatus(provider).source === "runtime"`: a runtime API key wins over
        // the stored accounts, so the first turn resolves the flat credential only.
        self.registry.get_provider_auth_status(provider).source.as_deref() != Some("runtime")
    }
}

pub fn credential_rotation_policy<'a>(
    registry: &'a dyn ModelRegistry,
    agent_dir: Option<&Path>,
) -> CredentialRotationPolicy<'a> {
    CredentialRotationPolicy {
        disabled: rotation_disabled_providers(agent_dir),
        registry,
    }
}

fn rotation_disabled_providers(agent_dir: Option<&Path>) -> BTreeSet<String> {
    let Some(agent_dir) = agent_dir else {
        return BTreeSet::new();
    };
    let parsed = utils::read_jsonc_file(agent_dir.join("models.json"));
    let Some(JsonValue::Object(root)) = parsed else {
        return BTreeSet::new();
    };
    let Some(JsonValue::Object(providers)) = root.get("providers") else {
        return BTreeSet::new();
    };
    let mut disabled = BTreeSet::new();
    for (id, entry) in providers {
        let Some(JsonValue::Object(credentials)) = entry.get("credentials") else {
            continue;
        };
        if credentials.get("rotation") == Some(&JsonValue::Bool(false)) {
            disabled.insert(id.clone());
        }
    }
    disabled
}
