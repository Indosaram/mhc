//! Shared memory component reference lifecycle.
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
pub struct MemoryModuleSupervisor {
    references: AtomicUsize,
}

pub static MEMORY_MODULE_SUPERVISOR: MemoryModuleSupervisor = MemoryModuleSupervisor {
    references: AtomicUsize::new(0),
};

impl MemoryModuleSupervisor {
    pub fn ref_count(&self) -> usize {
        self.references.load(Ordering::SeqCst)
    }

    pub fn acquire(&self) {
        self.references.fetch_add(1, Ordering::SeqCst);
    }

    pub fn release(&self) {
        self.references.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            Some(count.saturating_sub(1))
        }).unwrap_or_else(|count| count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_sessions_acquire_and_release() {
        let supervisor = MemoryModuleSupervisor::default();
        supervisor.acquire();
        supervisor.acquire();
        assert_eq!(supervisor.ref_count(), 2);
        supervisor.release();
        supervisor.release();
        assert_eq!(supervisor.ref_count(), 0);
        supervisor.release();
        assert_eq!(supervisor.ref_count(), 0);
    }
}
