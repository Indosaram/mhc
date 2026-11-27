use std::sync::Arc;

pub trait SessionSpawnAttestationHandle: Send + Sync {
    fn release(&self);
}

/// Attests a process this host spawned for a kernel to the session it runs for (`senpi#6972`).
pub type AttestKernelSpawn =
    Arc<dyn Fn(u32) -> Box<dyn SessionSpawnAttestationHandle> + Send + Sync>;
