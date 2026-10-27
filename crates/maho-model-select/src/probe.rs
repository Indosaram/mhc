//! Request-auth probe: skip candidates the first turn could not use (spec model-selection §6.6).

use crate::profile::{Exclusions, ProfileResolution};
use serde_json::Value;
use std::future::Future;
use std::path::Path;

/// Why a candidate was skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeReason {
    /// The stored login can no longer refresh (re-authenticate the provider).
    Refresh,
    /// No usable provider credential.
    Credentials,
    /// The model's request auth (key + headers) failed.
    Request,
}

impl ProbeReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeReason::Refresh => "refresh",
            ProbeReason::Credentials => "credentials",
            ProbeReason::Request => "request",
        }
    }
}

/// An auth error, reduced to what may be logged (never the message, which may hold secrets).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeError {
    pub class: Option<String>,
    pub code: Option<String>,
}

impl ProbeError {
    /// Fingerprint built from the class and machine code only.
    pub fn fingerprint(&self) -> String {
        format!(
            "{}:{}",
            self.class.as_deref().unwrap_or("error"),
            self.code.as_deref().unwrap_or("none")
        )
    }
}

/// One skipped candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeFailure {
    pub provider: String,
    pub model: String,
    pub reason: ProbeReason,
    pub fingerprint: String,
}

/// A credential account of a provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub pinned: bool,
}

/// What the probe needs from the host.
pub trait AuthProbe {
    /// Whether the provider may rotate between accounts.
    fn rotation_allowed(&self, provider: &str) -> impl Future<Output = bool> + Send;
    fn accounts(&self, provider: &str) -> impl Future<Output = Vec<Account>> + Send;
    /// Provider auth on a slot (`None` = the default credential).
    fn provider_auth(
        &self,
        provider: &str,
        slot: Option<&str>,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send;
    /// The model's request auth on the slot that succeeded.
    fn request_auth(
        &self,
        provider: &str,
        model: &str,
        slot: Option<&str>,
    ) -> impl Future<Output = Result<(), ProbeError>> + Send;
}

/// Credential slots to try for a provider.
pub async fn credential_slots<P: AuthProbe + Sync>(
    probe: &P,
    provider: &str,
) -> Vec<Option<String>> {
    if !probe.rotation_allowed(provider).await {
        return vec![None];
    }
    let accounts = probe.accounts(provider).await;
    if let Some(pinned) = accounts.iter().find(|a| a.pinned) {
        return vec![Some(pinned.name.clone())];
    }
    if accounts.is_empty() {
        vec![None]
    } else {
        accounts.into_iter().map(|a| Some(a.name)).collect()
    }
}

const MAX_ROUNDS: usize = 64;

/// Resolves, probes the candidate, excludes what fails and re-resolves until a candidate passes or
/// nothing resolves. Pins are never probed.
pub async fn probe_and_resolve<P, R>(
    probe: &P,
    resolve: R,
) -> (ProfileResolution, Vec<ProbeFailure>)
where
    P: AuthProbe + Sync,
    R: Fn(&Exclusions) -> ProfileResolution,
{
    let mut exclusions = Exclusions::default();
    let mut failures = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let result = resolve(&exclusions);
        let ProfileResolution::Resolved {
            provider,
            model_id,
            pinned: false,
            ..
        } = &result
        else {
            return (result, failures);
        };
        let mut passed_slot = None;
        let mut last_error = ProbeError::default();
        for slot in credential_slots(probe, provider).await {
            match probe.provider_auth(provider, slot.as_deref()).await {
                Ok(()) => {
                    passed_slot = Some(slot);
                    break;
                }
                Err(error) => last_error = error,
            }
        }
        let Some(slot) = passed_slot else {
            let reason = if last_error.code.as_deref() == Some("oauth") {
                ProbeReason::Refresh
            } else {
                ProbeReason::Credentials
            };
            failures.push(ProbeFailure {
                provider: provider.clone(),
                model: model_id.clone(),
                reason,
                fingerprint: last_error.fingerprint(),
            });
            exclusions.providers.insert(provider.clone());
            continue;
        };
        if let Err(error) = probe
            .request_auth(provider, model_id, slot.as_deref())
            .await
        {
            failures.push(ProbeFailure {
                provider: provider.clone(),
                model: model_id.clone(),
                reason: ProbeReason::Request,
                fingerprint: error.fingerprint(),
            });
            exclusions.models.insert(format!("{provider}/{model_id}"));
            continue;
        }
        return (result, failures);
    }
    (resolve(&exclusions), failures)
}

/// Whether `<agent dir>/models.json` allows credential rotation for `provider` (an unreadable
/// file allows it).
pub fn rotation_allowed_by_models_json(agent_dir: &Path, provider: &str) -> bool {
    let Some(value) = maho_support::read_jsonc_file(&agent_dir.join("models.json")) else {
        return true;
    };
    value
        .get("providers")
        .and_then(|p| p.get(provider))
        .and_then(|p| p.get("credentials"))
        .and_then(|c| c.get("rotation"))
        != Some(&Value::Bool(false))
}
