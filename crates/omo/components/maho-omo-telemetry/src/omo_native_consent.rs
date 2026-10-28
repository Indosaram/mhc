//! Port of the consent half of `packages/omo-senpi/src/components/telemetry/omo-native-session.ts` and
//! `omo-native-component.ts`: the persisted-config gate, the per-process env opt-outs, and the writer's
//! config selection a queued record carries.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::index::SenpiTelemetryOptions;
use crate::omo_native_session::{
    OPTED_OUT_SESSION_HASH, is_omo_native_telemetry_config_enabled,
    is_omo_native_telemetry_enabled,
};
use crate::product_identity::hash_session_id;
use crate::session_summary_purge::{ConfigSelection, config_selection_of, with_config_selection};
use crate::session_summary_store::ConsentScope;

/// A consent read is reused for this long on the per-turn path; the periodic check always reads fresh.
pub const CONSENT_CACHE_MS: f64 = 5_000.0;

struct ConsentCache {
    cwd: Option<PathBuf>,
    at: f64,
    value: bool,
}

/// Read consent at the forwarding boundary, not just when the session client is created.
pub struct NativeConsent {
    options: SenpiTelemetryOptions,
    env: telemetry_core::TelemetryEnv,
    cwd: Mutex<Option<PathBuf>>,
    cache: Mutex<ConsentCache>,
    drop_queued: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

fn clock_of(options: &SenpiTelemetryOptions) -> Arc<dyn Fn() -> f64 + Send + Sync> {
    options.consent_now.clone().unwrap_or_else(|| {
        Arc::new(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0
        })
    })
}

impl NativeConsent {
    pub fn new(options: &SenpiTelemetryOptions) -> Arc<Self> {
        let env = options
            .env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        Arc::new(Self {
            options: options.clone(),
            env,
            cwd: Mutex::new(None),
            cache: Mutex::new(ConsentCache {
                cwd: None,
                at: 0.0,
                value: false,
            }),
            drop_queued: Mutex::new(None),
        })
    }

    pub fn set_cwd(&self, cwd: &Path) {
        *self.cwd.lock().unwrap_or_else(PoisonError::into_inner) = Some(cwd.to_owned());
    }

    pub fn cwd(&self) -> Option<PathBuf> {
        self.cwd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_drop_queued(&self, drop_queued: Arc<dyn Fn() + Send + Sync>) {
        *self
            .drop_queued
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(drop_queued);
    }

    /// The single gate: the env opt-out matrix, the automation gate and the omo config gate for `cwd`.
    pub fn read_consent(&self, cwd: Option<&Path>) -> bool {
        is_omo_native_telemetry_enabled(&self.options, cwd, &self.env)
    }

    /// A cached read for the per-turn path; `fresh` always reads the config again.
    pub fn is_consented(&self, fresh: bool) -> bool {
        let cwd = self.cwd();
        let at = (clock_of(&self.options))();
        {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if !fresh && cache.cwd == cwd && at - cache.at < CONSENT_CACHE_MS {
                return cache.value;
            }
        }
        let was_consented = {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            cache.value
        };
        let value = self.read_consent(cwd.as_deref());
        {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            *cache = ConsentCache {
                cwd,
                at,
                value,
            };
        }
        // Events queued before the withdrawal are dropped the moment it is read, not sent by a later flush.
        if was_consented && !value {
            let drop_queued = self
                .drop_queued
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(drop_queued) = drop_queued {
                drop_queued();
            }
        }
        value
    }

    /// The gate for a specific cwd, bypassing the cache.
    pub fn allowed_for(&self, cwd: &Path) -> bool {
        self.read_consent(Some(cwd))
    }

    pub fn allowed(&self) -> bool {
        self.is_consented(false)
    }

    /// A record is sent, resumed or kept by its writer's config, with this process's own env opt-outs.
    pub fn is_scope_consented(&self, scope: &ConsentScope) -> bool {
        let env = match &scope.config {
            None => self.env.clone(),
            Some(selection) => with_config_selection(&self.env, selection),
        };
        is_omo_native_telemetry_enabled(
            &self.options,
            scope.cwd.as_deref().map(Path::new),
            &env,
        )
    }

    /// Consent for the shared queue: the persisted setting only, read with a neutral env. An opt-out set by
    /// environment variable is this process's own and must not delete what consented sessions queued.
    pub fn is_queue_scope_consented(&self, scope: &ConsentScope) -> bool {
        is_omo_native_telemetry_config_enabled(
            &self.options,
            scope.cwd.as_deref().map(Path::new),
            &self.env,
            self.options.account_home_dir.as_deref(),
            scope.config.as_ref(),
        )
    }

    /// True when this process's own persisted setting is off: unreadable summary files are deleted too.
    pub fn persisted_opt_out(&self) -> bool {
        !self.is_queue_scope_consented(&self.consent_scope())
    }

    pub fn config_selection(&self) -> ConfigSelection {
        config_selection_of(&self.env)
    }

    /// The scope this process's consent was read for; stamped into every persisted record.
    pub fn consent_scope(&self) -> ConsentScope {
        ConsentScope {
            cwd: self.cwd().map(|cwd| cwd.to_string_lossy().into_owned()),
            config: Some(self.config_selection()),
        }
    }

    /// What a session hashes to while consent is off: no salt is read or created, and no event carrying it is
    /// sent.
    pub fn hash_session(&self, raw_id: &str, state_dir: &Path) -> String {
        if !self.is_consented(false) {
            return OPTED_OUT_SESSION_HASH.to_string();
        }
        hash_session_id(raw_id, state_dir).unwrap_or_else(|error| {
            maho_ext_api::diagnostic(&format!("omo-native session identity failed: {error}"));
            String::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn options(enabled: Arc<AtomicBool>) -> SenpiTelemetryOptions {
        SenpiTelemetryOptions {
            env: Some(
                [("POSTHOG_API_KEY".to_string(), "fixture-key".to_string())]
                    .into_iter()
                    .collect(),
            ),
            is_config_enabled: Some(Arc::new(move |_| enabled.load(Ordering::SeqCst))),
            consent_now: Some(Arc::new(|| 1000.0)),
            ..Default::default()
        }
    }

    #[test]
    fn consent_is_cached_and_a_withdrawal_drops_queued_events_once() {
        let enabled = Arc::new(AtomicBool::new(true));
        let consent = NativeConsent::new(&options(Arc::clone(&enabled)));
        consent.set_cwd(Path::new("/repo"));
        assert!(consent.is_consented(false));
        enabled.store(false, Ordering::SeqCst);
        // The cached read still answers true inside the window.
        assert!(consent.is_consented(false));
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&drops);
        consent.set_drop_queued(Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(!consent.is_consented(true));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!consent.is_consented(true));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_scope_carries_the_writer_config_selection() {
        let enabled = Arc::new(AtomicBool::new(true));
        let mut options = options(enabled);
        options
            .env
            .as_mut()
            .unwrap()
            .insert("OMO_PROFILE".into(), "work".into());
        let consent = NativeConsent::new(&options);
        consent.set_cwd(Path::new("/repo"));
        let scope = consent.consent_scope();
        assert_eq!(scope.cwd.as_deref(), Some("/repo"));
        assert_eq!(
            scope
                .config
                .as_ref()
                .and_then(|config| config.get("OMO_PROFILE"))
                .map(String::as_str),
            Some("work")
        );
        assert!(consent.is_scope_consented(&scope));
    }

    #[test]
    fn the_writer_selection_judges_the_record() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&seen);
        let mut options = options(Arc::new(AtomicBool::new(true)));
        options.is_config_enabled = Some(Arc::new(move |_| {
            observed.lock().unwrap().push(());
            true
        }));
        let consent = NativeConsent::new(&options);
        let scope = ConsentScope {
            cwd: Some("/repo".into()),
            config: Some(
                [("OMO_PROFILE".to_string(), "writer".to_string())]
                    .into_iter()
                    .collect(),
            ),
        };
        assert!(consent.is_scope_consented(&scope));
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn an_opted_out_session_hashes_to_the_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        let enabled = Arc::new(AtomicBool::new(false));
        let consent = NativeConsent::new(&options(enabled));
        consent.set_cwd(Path::new("/repo"));
        assert_eq!(
            consent.hash_session("raw", dir.path()),
            OPTED_OUT_SESSION_HASH
        );
        assert!(!dir.path().join("session-id-salt").exists());
    }

    #[test]
    fn an_opted_in_session_hashes_with_the_salt() {
        let dir = tempfile::tempdir().unwrap();
        let consent = NativeConsent::new(&options(Arc::new(AtomicBool::new(true))));
        consent.set_cwd(Path::new("/repo"));
        let hash = consent.hash_session("raw", dir.path());
        assert_eq!(hash.len(), 64);
        assert_ne!(hash, OPTED_OUT_SESSION_HASH);
        assert!(dir.path().join("session-id-salt").exists());
    }
}
