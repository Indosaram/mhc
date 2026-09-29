#![allow(
    dead_code,
    reason = "each test module uses a different subset of helpers"
)]

use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;

use indexmap::IndexSet;
use model_core::_set_model_resolution_log_implementation_for_testing;
use model_core::FallbackEntry;
use model_core::ModelCapabilitiesSnapshot;
use model_core::ModelMetadata;
use model_core::ProviderCache;
use model_core::get_bundled_model_capabilities_snapshot;
use serde_json::Value;

pub fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

pub fn set(values: &[&str]) -> IndexSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

pub fn entry(providers: &[&str], model: &str, variant: Option<&str>) -> FallbackEntry {
    FallbackEntry {
        providers: strings(providers),
        model: model.to_string(),
        variant: variant.map(str::to_string),
        ..FallbackEntry::default()
    }
}

/// The generated models.dev snapshot the TS tests import from omo-opencode.
pub static GENERATED_SNAPSHOT_JSON: LazyLock<ModelCapabilitiesSnapshot> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../fixtures/model-capabilities.generated.json"
    ))
    .unwrap_or_else(|error| panic!("fixture must parse: {error}"))
});

pub fn bundled_snapshot() -> ModelCapabilitiesSnapshot {
    get_bundled_model_capabilities_snapshot(&GENERATED_SNAPSHOT_JSON)
}

/// Stand-in for spying on `readConnectedProvidersCache`.
pub struct ConnectedCache(pub Option<Vec<String>>);

impl ConnectedCache {
    pub fn with(providers: &[&str]) -> Self {
        Self(Some(strings(providers)))
    }
}

impl ProviderCache for ConnectedCache {
    fn read_connected_providers_cache(&self) -> Option<Vec<String>> {
        self.0.clone()
    }

    fn find_provider_model_metadata(
        &self,
        _provider_id: &str,
        _model_id: &str,
    ) -> Option<ModelMetadata> {
        None
    }
}

/// Provider cache answering metadata lookups by exact model id.
pub struct MetadataCache(pub Vec<(&'static str, Value)>);

impl ProviderCache for MetadataCache {
    fn read_connected_providers_cache(&self) -> Option<Vec<String>> {
        None
    }

    fn find_provider_model_metadata(
        &self,
        _provider_id: &str,
        model_id: &str,
    ) -> Option<ModelMetadata> {
        self.0
            .iter()
            .find(|(id, _)| *id == model_id)
            .and_then(|(_, metadata)| metadata.as_object().cloned())
    }
}

pub type LogCalls = Arc<Mutex<Vec<(String, Option<Value>)>>>;

static LOG_HOOK_LOCK: Mutex<()> = Mutex::new(());

/// Installs the pipeline log hook for one test (serialized, cleared on drop).
pub struct LogCapture {
    calls: LogCalls,
    _guard: MutexGuard<'static, ()>,
}

impl LogCapture {
    pub fn install() -> Self {
        let guard = LOG_HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let calls: LogCalls = Arc::default();
        let sink = Arc::clone(&calls);
        _set_model_resolution_log_implementation_for_testing(Some(Box::new(
            move |message, data| {
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((message.to_string(), data.cloned()));
            },
        )));
        Self {
            calls,
            _guard: guard,
        }
    }

    pub fn was_called_with(&self, message: &str, data: Option<&Value>) -> bool {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(logged, logged_data)| logged == message && logged_data.as_ref() == data)
    }

    pub fn was_called_with_message(&self, message: &str) -> bool {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(logged, _)| logged == message)
    }
}

impl Drop for LogCapture {
    fn drop(&mut self) {
        _set_model_resolution_log_implementation_for_testing(None);
    }
}
