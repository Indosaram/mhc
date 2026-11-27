//! senpi `modes/rpc/compact-runtime-factory.ts`: marks a preparable runtime factory as supporting
//! compact updates (only the CLI factory constructs its own internal Agent channel for every session).

use crate::host_warm::PrepareRuntime;

#[derive(Clone)]
pub struct CompactRuntimeFactory {
    pub prepare: Option<PrepareRuntime>,
    pub supports_compact: bool,
}

impl CompactRuntimeFactory {
    pub fn new(prepare: Option<PrepareRuntime>, supports_compact: bool) -> Self {
        Self {
            prepare,
            supports_compact,
        }
    }
}

pub fn complete_compact_factory(prepare: PrepareRuntime) -> CompactRuntimeFactory {
    CompactRuntimeFactory {
        prepare: Some(prepare),
        supports_compact: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn marks_factory_compact_capable() {
        let prepare: PrepareRuntime = Arc::new(|_| Box::pin(async { Ok(()) }));
        let factory = complete_compact_factory(prepare);
        assert!(factory.supports_compact);
        assert!(factory.prepare.is_some());
    }
}
