use crate::tool::types::EvalLanguage;
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelMeasure {
    Heap,
    Footprint,
}

/// Backed by the kernel, rather than a particular worker generation.
pub trait RegisteredKernelSource: Send + Sync {
    fn measure(&self) -> KernelMeasure;
    fn last_live_bytes(&self) -> Option<u64>;
    fn busy(&self) -> bool;
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
        let mut kernels = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = kernels.iter_mut().find(|entry| entry.id == kernel.id) {
            *existing = kernel;
        } else {
            kernels.push(kernel);
        }
    }

    pub fn unregister(&self, id: &str) -> bool {
        let mut kernels = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = kernels.len();
        kernels.retain(|entry| entry.id != id);
        kernels.len() != before
    }

    pub fn list(&self) -> Vec<KernelListing> {
        let kernels = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
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
}

/// Shared by native extension registrations and host diagnostic consumers.
pub static KERNEL_REGISTRY: LazyLock<KernelRegistry> = LazyLock::new(KernelRegistry::default);
