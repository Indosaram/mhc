use super::kernel_registry::{
    KERNEL_REGISTRY, KernelMeasure, KernelRegistration, RegisteredKernelSource,
};
use crate::{kernels::js::context_manager::JavaScriptKernel, tool::types::EvalLanguage};
use std::{collections::HashMap, sync::Arc};

pub fn register_kernel(
    session_id: &str,
    language: EvalLanguage,
    source: Arc<dyn RegisteredKernelSource>,
) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    KERNEL_REGISTRY.register(KernelRegistration {
        id: id.clone(),
        session_id: session_id.into(),
        language,
        source,
    });
    id
}

/// Session ownership also unregisters entries when a manager is dropped without disposal.
pub struct SessionKernelRegistrations {
    owner: String,
    ids: HashMap<EvalLanguage, String>,
}

impl SessionKernelRegistrations {
    pub fn new(owner: String) -> Self {
        Self {
            owner,
            ids: HashMap::new(),
        }
    }

    pub fn register(
        &mut self,
        language: EvalLanguage,
        source: Option<Arc<dyn RegisteredKernelSource>>,
    ) {
        self.unregister(language);
        if let Some(source) = source {
            self.ids
                .insert(language, register_kernel(&self.owner, language, source));
        }
    }

    pub fn unregister(&mut self, language: EvalLanguage) {
        if let Some(id) = self.ids.remove(&language) {
            KERNEL_REGISTRY.unregister(&id);
        }
    }

    pub fn clear(&mut self) {
        for (_, id) in self.ids.drain() {
            KERNEL_REGISTRY.unregister(&id);
        }
    }
}

impl Drop for SessionKernelRegistrations {
    fn drop(&mut self) {
        self.clear();
    }
}

pub fn java_script_kernel_memory(kernel: Arc<JavaScriptKernel>) -> Arc<dyn RegisteredKernelSource> {
    kernel
}

impl RegisteredKernelSource for JavaScriptKernel {
    fn measure(&self) -> KernelMeasure {
        self.memory_measure()
    }
    fn last_live_bytes(&self) -> Option<u64> {
        self.last_live_bytes()
    }
    fn busy(&self) -> bool {
        self.queue_snapshot().0.is_some()
    }
    fn pid(&self) -> Option<u32> {
        match self.memory_measure() {
            KernelMeasure::Heap => None,
            KernelMeasure::Footprint => self.pid(),
        }
    }
}
