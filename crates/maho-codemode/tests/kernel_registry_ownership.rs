use maho_codemode::{
    extension::{
        kernel_registration::SessionKernelRegistrations,
        kernel_registry::{
            KERNEL_REGISTRY, KernelMeasure, KernelMemoryReading, MemoryQuery,
            RegisteredKernelSource,
        },
    },
    tool::types::EvalLanguage,
};
use std::sync::Arc;

struct MemorySource;
impl RegisteredKernelSource for MemorySource {
    fn measure(&self) -> KernelMeasure {
        KernelMeasure::Heap
    }
    fn last_live_bytes(&self) -> Option<u64> {
        Some(42)
    }
    fn busy(&self) -> bool {
        false
    }
    fn query_memory(&self) -> MemoryQuery<'_> {
        Box::pin(async {
            Ok(Some(KernelMemoryReading {
                live_bytes: 42,
                measure: KernelMeasure::Heap,
            }))
        })
    }
}

#[tokio::test]
async fn replacement_and_drop_remove_session_owned_entries() {
    let session = uuid::Uuid::new_v4().to_string();
    let rows = || {
        KERNEL_REGISTRY
            .list()
            .into_iter()
            .filter(|row| row.session_id == session)
            .collect::<Vec<_>>()
    };
    let mut owner = SessionKernelRegistrations::new(session.clone());
    owner.register(EvalLanguage::Js, Some(Arc::new(MemorySource)));
    let old = rows().pop().expect("registered").id;
    assert_eq!(
        KERNEL_REGISTRY
            .query(&old)
            .await
            .expect("query")
            .expect("reading")
            .live_bytes,
        42
    );
    owner.register(EvalLanguage::Js, Some(Arc::new(MemorySource)));
    let replacement = rows();
    assert_eq!(replacement.len(), 1);
    assert_ne!(replacement[0].id, old);
    assert!(
        KERNEL_REGISTRY
            .query(&old)
            .await
            .expect("removed query")
            .is_none()
    );
    drop(owner);
    assert!(rows().is_empty());
}
