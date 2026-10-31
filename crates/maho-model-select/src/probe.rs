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

/// How an OAuth refresh failed (spec model-selection Delta D4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefreshFailure {
    /// The token endpoint answered and rejected the stored grant.
    Refused,
    /// The endpoint could not be reached, timed out or answered with a temporary error.
    /// `transport` is a short classification (an OS error code, `http_<status>`, a timeout
    /// marker), never a message, URL or body.
    Transient { transport: String },
}

/// An auth error, reduced to what may be logged (never the message, which may hold secrets).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProbeError {
    pub class: Option<String>,
    pub code: Option<String>,
    /// The OAuth refresh split, when the host reports it on this error.
    pub refresh: Option<RefreshFailure>,
    /// The wrapped error, when this error wraps another one.
    pub cause: Option<Box<ProbeError>>,
}

impl ProbeError {
    /// The refresh split carried by this error or any error it wraps (outermost first).
    pub fn refresh_failure(&self) -> Option<&RefreshFailure> {
        let mut current = Some(self);
        while let Some(error) = current {
            if let Some(refresh) = &error.refresh {
                return Some(refresh);
            }
            current = error.cause.as_deref();
        }
        None
    }

    /// The transport classification of a transient refresh failure anywhere in the chain.
    pub fn transient_transport(&self) -> Option<&str> {
        match self.refresh_failure()? {
            RefreshFailure::Transient { transport } => Some(transport.as_str()),
            RefreshFailure::Refused => None,
        }
    }

    /// Whether this is an OAuth refresh failure that is not transient (reason `refresh`).
    fn is_refused_refresh(&self) -> bool {
        matches!(self.refresh_failure(), Some(RefreshFailure::Refused))
            || (self.refresh_failure().is_none() && self.code.as_deref() == Some("oauth"))
    }

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

/// A candidate kept despite a transient OAuth refresh failure (Delta D4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptCandidate {
    pub provider: String,
    pub model: String,
    pub fingerprint: String,
    pub transport: String,
    /// Whether the model-scope (request auth) check completed.
    pub model_checked: bool,
}

impl KeptCandidate {
    /// The warning log line: provider, model, fingerprint and transport only.
    pub fn warning_line(&self) -> String {
        let scope = if self.model_checked {
            ""
        } else {
            ", model scope unchecked"
        };
        format!(
            "kept {}/{} after a transient login refresh failure ({}, {}{scope})",
            self.provider, self.model, self.fingerprint, self.transport
        )
    }
}

/// Everything the probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub result: ProfileResolution,
    pub failures: Vec<ProbeFailure>,
    pub kept: Vec<KeptCandidate>,
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
/// nothing resolves. Pins are never probed. Kept candidates (transient refresh) are logged as
/// warnings; use [`probe_and_resolve_detailed`] to inspect them.
pub async fn probe_and_resolve<P, R>(
    probe: &P,
    resolve: R,
) -> (ProfileResolution, Vec<ProbeFailure>)
where
    P: AuthProbe + Sync,
    R: Fn(&Exclusions) -> ProfileResolution,
{
    let outcome = probe_and_resolve_detailed(probe, resolve).await;
    for kept in &outcome.kept {
        maho_support::log(&kept.warning_line(), None);
    }
    (outcome.result, outcome.failures)
}

/// [`probe_and_resolve`] returning the kept candidates too (Delta D4): a transient OAuth refresh
/// failure keeps the provider (no `refresh` failure); only a refused refresh excludes it.
pub async fn probe_and_resolve_detailed<P, R>(probe: &P, resolve: R) -> ProbeOutcome
where
    P: AuthProbe + Sync,
    R: Fn(&Exclusions) -> ProfileResolution,
{
    let mut exclusions = Exclusions::default();
    let mut failures = Vec::new();
    let mut kept = Vec::new();
    for _ in 0..MAX_ROUNDS {
        let result = resolve(&exclusions);
        let ProfileResolution::Resolved {
            provider,
            model_id,
            pinned: false,
            ..
        } = &result
        else {
            return ProbeOutcome {
                result,
                failures,
                kept,
            };
        };
        let mut passed_slot = None;
        let mut transient: Option<(Option<String>, ProbeError)> = None;
        let mut last_error = ProbeError::default();
        for slot in credential_slots(probe, provider).await {
            match probe.provider_auth(provider, slot.as_deref()).await {
                Ok(()) => {
                    passed_slot = Some(slot);
                    break;
                }
                Err(error) => {
                    if transient.is_none() && error.transient_transport().is_some() {
                        transient = Some((slot, error.clone()));
                    }
                    last_error = error;
                }
            }
        }
        let (slot, transient_error) = match (passed_slot, transient) {
            (Some(slot), _) => (slot, None),
            (None, Some((slot, error))) => (slot, Some(error)),
            (None, None) => {
                let reason = if last_error.is_refused_refresh() {
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
            }
        };
        let mut model_checked = true;
        if let Err(error) = probe
            .request_auth(provider, model_id, slot.as_deref())
            .await
        {
            if transient_error.is_some() && error.transient_transport().is_some() {
                model_checked = false;
            } else {
                failures.push(ProbeFailure {
                    provider: provider.clone(),
                    model: model_id.clone(),
                    reason: ProbeReason::Request,
                    fingerprint: error.fingerprint(),
                });
                exclusions.models.insert(format!("{provider}/{model_id}"));
                continue;
            }
        }
        if let Some(error) = transient_error {
            kept.push(KeptCandidate {
                provider: provider.clone(),
                model: model_id.clone(),
                fingerprint: error.fingerprint(),
                transport: error.transient_transport().unwrap_or_default().to_owned(),
                model_checked,
            });
        }
        return ProbeOutcome {
            result,
            failures,
            kept,
        };
    }
    ProbeOutcome {
        result: resolve(&exclusions),
        failures,
        kept,
    }
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
