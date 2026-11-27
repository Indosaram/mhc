//! Port of `rpc-remote-source.ts` (senpi#6151):
//! Seam for watching RPC sessions on another device (`<device>/<session>`, `<device>/*`).
//! Cross-device monitor targets are deferred to a later port, so
//! `current_rpc_remote_source()` returns `None` in production while preserving the full
//! target/checkpoint types and watch plumbing.

use crate::rpc_monitor_targets::RpcEvents;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRemoteTarget {
    pub device: String,
    pub session: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRemoteSettledTurn {
    pub session_id: String,
    pub turn_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RpcRemoteState {
    pub rows: Vec<Value>,
    pub settled_turns: Option<Vec<RpcRemoteSettledTurn>>,
    pub generation: Option<String>,
    pub cursor: Option<String>,
}

pub struct RpcRemoteHandlers {
    pub record: Box<dyn Fn(Value) + Send + Sync>,
    pub resynced: Box<dyn Fn(RpcRemoteState) + Send + Sync>,
    pub cursor: Box<dyn Fn(String) + Send + Sync>,
    pub lost: Box<dyn Fn(String) + Send + Sync>,
    pub ended: Box<dyn Fn(String) + Send + Sync>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcRemoteOpenOptions {
    pub events: RpcEvents,
    pub exclude: Vec<String>,
    pub resume_from: Option<String>,
}

pub struct RpcRemoteSubscription {
    pub initial: RpcRemoteState,
    stop: Box<dyn Fn() + Send + Sync>,
}

impl RpcRemoteSubscription {
    pub fn new(initial: RpcRemoteState, stop: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            initial,
            stop: Box::new(stop),
        }
    }

    pub fn stop(&self) {
        (self.stop)();
    }
}

pub type RpcRemoteOpenFuture =
    Pin<Box<dyn Future<Output = Result<RpcRemoteSubscription, String>> + Send>>;

pub trait RpcRemoteSource: Send + Sync {
    fn open(
        &self,
        target: RpcRemoteTarget,
        options: RpcRemoteOpenOptions,
        handlers: RpcRemoteHandlers,
    ) -> RpcRemoteOpenFuture;
}

pub type SharedRpcRemoteSource = Arc<dyn RpcRemoteSource>;

/// Cross-device `<device>/<session>` remote sources are deferred to a later port.
/// Returns `None` so local monitors run normally and remote targets fail closed with the
/// upstream refusal message.
pub fn current_rpc_remote_source() -> Option<SharedRpcRemoteSource> {
    None
}

#[cfg(test)]
pub(crate) fn tests_helper_source() -> impl RpcRemoteSource {
    struct DummySource;
    impl RpcRemoteSource for DummySource {
        fn open(
            &self,
            _target: RpcRemoteTarget,
            _options: RpcRemoteOpenOptions,
            _handlers: RpcRemoteHandlers,
        ) -> RpcRemoteOpenFuture {
            Box::pin(async { Err("dummy".to_owned()) })
        }
    }
    DummySource
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_remote_source_is_none_by_default() {
        assert!(current_rpc_remote_source().is_none());
    }
}
