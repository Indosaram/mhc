use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use crate::{index::SenpiTelemetryOptions, omo_native_component::ConfigEnabled};

/// Read consent at the forwarding boundary, not just when the session client is created.
pub struct NativeConsent {
    cwd: Mutex<Option<PathBuf>>,
    env: Option<telemetry_core::TelemetryEnv>,
    enabled: ConfigEnabled,
}

impl NativeConsent {
    pub fn new(options: &SenpiTelemetryOptions, enabled: ConfigEnabled) -> Arc<Self> {
        Arc::new(Self {
            cwd: Mutex::new(None),
            env: options.env.clone(),
            enabled,
        })
    }

    pub fn set_cwd(&self, cwd: &std::path::Path) {
        *self.cwd.lock().unwrap_or_else(PoisonError::into_inner) = Some(cwd.to_owned());
    }

    pub fn allowed(&self) -> bool {
        let cwd = self
            .cwd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        cwd.is_some_and(|cwd| self.allowed_for(&cwd))
    }

    pub fn allowed_for(&self, cwd: &std::path::Path) -> bool {
        let env = self
            .env
            .clone()
            .unwrap_or_else(|| std::env::vars().collect());
        let product = crate::product_identity::create_omo_native_product_config();
        telemetry_core::is_telemetry_client_enabled(
            &telemetry_core::TelemetryClientEnabledInput::for_product(Some(&env), &product),
        ) && (self.enabled)(cwd)
    }
}
