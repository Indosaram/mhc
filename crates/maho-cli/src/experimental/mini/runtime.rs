//! Shared, mutable access to the worker's model runtime.
//!
//! The worker owns one `ModelRuntime`; the `Models` service reads it and refreshes it, and the `Lane`
//! service resolves model identities against it. `ModelRuntime::refresh` takes `&mut self`, so the
//! runtime is serialized behind one async mutex and every consumer goes through this handle.

use std::sync::Arc;

use maho_ai::models::{ModelsRefreshOptions, ModelsRefreshResult, Provider};
use maho_ai::types::Model;
use maho_core::model_runtime::ModelRuntime;

pub struct ModelRuntimeHandle {
    inner: tokio::sync::Mutex<ModelRuntime>,
}

impl ModelRuntimeHandle {
    pub fn new(runtime: ModelRuntime) -> Arc<Self> {
        Arc::new(Self { inner: tokio::sync::Mutex::new(runtime) })
    }

    pub async fn available(&self) -> Vec<Model> {
        self.inner.lock().await.get_available(None).await
    }

    pub async fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.inner.lock().await.get_providers()
    }

    pub async fn auth_status(&self, id: &str) -> (bool, Option<String>, Option<String>) {
        let status = self.inner.lock().await.get_provider_auth_status(id);
        (status.configured, status.source, status.label)
    }

    pub async fn has_model(&self, provider: &str, id: &str) -> bool {
        self.inner.lock().await.get_model(provider, id).is_some()
    }

    pub async fn refresh(&self, options: ModelsRefreshOptions) -> ModelsRefreshResult {
        self.inner.lock().await.refresh(options).await
    }
}
