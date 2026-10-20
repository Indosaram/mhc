use crate::tool::types::EvalLanguage;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock, Mutex},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelMeasure {
    Heap,
    Footprint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelMemoryReading {
    pub live_bytes: u64,
    pub measure: KernelMeasure,
}

pub type MemoryQuery<'a> =
    Pin<Box<dyn Future<Output = Result<Option<KernelMemoryReading>, String>> + Send + 'a>>;

/// Backed by the kernel, rather than a particular worker generation.
pub trait RegisteredKernelSource: Send + Sync {
    fn measure(&self) -> KernelMeasure;
    fn last_live_bytes(&self) -> Option<u64>;
    fn busy(&self) -> bool;
    fn query_memory(&self) -> MemoryQuery<'_>;
    fn pid(&self) -> Option<u32> {
        None
    }
}

#[derive(Clone)]
pub struct KernelRegistration {
    pub id: String,
    pub session_id: String,
    pub language: EvalLanguage,
    pub source: Arc<dyn RegisteredKernelSource>,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KernelListing {
    pub id: String,
    pub session_id: String,
    pub language: EvalLanguage,
    pub measure: KernelMeasure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_live_bytes: Option<u64>,
    /// Host reports map this to `stale`: the cached reading may predate the active cell.
    pub busy: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Default)]
pub struct KernelRegistry(Mutex<Vec<KernelRegistration>>);

impl KernelRegistry {
    pub fn register(&self, kernel: KernelRegistration) {
        let mut kernels = self.0.lock().expect("kernel registry lock");
        if let Some(existing) = kernels.iter_mut().find(|entry| entry.id == kernel.id) {
            *existing = kernel;
        } else {
            kernels.push(kernel);
        }
    }

    pub fn unregister(&self, id: &str) -> bool {
        let mut kernels = self.0.lock().expect("kernel registry lock");
        let before = kernels.len();
        kernels.retain(|entry| entry.id != id);
        kernels.len() != before
    }

    pub fn list(&self) -> Vec<KernelListing> {
        let kernels = self.0.lock().expect("kernel registry lock").clone();
        kernels
            .into_iter()
            .map(|entry| KernelListing {
                measure: entry.source.measure(),
                last_live_bytes: entry.source.last_live_bytes(),
                busy: entry.source.busy(),
                pid: entry.source.pid(),
                id: entry.id,
                session_id: entry.session_id,
                language: entry.language,
            })
            .collect()
    }

    fn source(&self, id: &str) -> Option<Arc<dyn RegisteredKernelSource>> {
        self.0
            .lock()
            .expect("kernel registry lock")
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.source.clone())
    }

    pub fn last_live_bytes(&self, id: &str) -> Option<u64> {
        self.source(id)?.last_live_bytes()
    }

    pub async fn query(&self, id: &str) -> Result<Option<KernelMemoryReading>, String> {
        match self.source(id) {
            Some(source) => source.query_memory().await,
            None => Ok(None),
        }
    }
}

/// Shared by native extension registrations and host diagnostic consumers.
pub static KERNEL_REGISTRY: LazyLock<KernelRegistry> = LazyLock::new(KernelRegistry::default);
