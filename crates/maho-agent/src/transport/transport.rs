//! Port of senpi `packages/agent/src/transport/transport.ts`.

use std::any::Any;
use std::sync::Arc;

use maho_ai::types::BoxFuture;

/// Opaque carrier message (in process, local socket, relay).
#[derive(Clone)]
pub struct CarrierMessage(Arc<dyn Any + Send + Sync>);

impl CarrierMessage {
    pub fn new<T: Send + Sync + 'static>(value: T) -> Self {
        Self(Arc::new(value))
    }

    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for CarrierMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CarrierMessage").finish_non_exhaustive()
    }
}

/// One delivered frame: the lane it was sent on and its opaque payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportFrame {
    pub lane: String,
    pub payload: Vec<u8>,
}

/// Receives frames. A lane's next frame is delivered only after this settles for the previous one;
/// lanes are independent.
pub type FrameHandler = Arc<dyn Fn(TransportFrame) -> BoxFuture<'static, ()> + Send + Sync>;

/// One endpoint of a transport link.
pub trait Transport: Send + Sync {
    /// Queues `payload` on `lane`. Resolves once the frame is inside the lane's send window.
    fn send(&self, lane: &str, payload: Vec<u8>)
    -> BoxFuture<'_, Result<(), TransportClosedError>>;
    /// Sets the receiver. Frames that arrived earlier are delivered once it is set.
    fn on_frame(&self, handler: FrameHandler);
    /// Frames on `lane` that were sent and not yet acknowledged by the peer.
    fn in_flight(&self, lane: &str) -> usize;
    fn close(&self);
}

/// A message channel under a transport.
pub trait CarrierConnection: Send + Sync {
    /// Hands `message` to the carrier. `false` means it was not taken; wait for `writable()`.
    fn send(&self, message: CarrierMessage) -> bool;
    fn close(&self);
}

/// What a carrier calls on the endpoint it serves.
pub trait CarrierSide: Send + Sync {
    /// A new connection is up (the first, or a re-attach after a drop).
    fn attached(&self, connection: Arc<dyn CarrierConnection>);
    fn message(&self, message: &CarrierMessage);
    /// The current connection takes messages again after a refused `send`.
    fn writable(&self);
    /// The current connection is gone; anything in flight on it is lost.
    fn detached(&self);
}

/// A transport endpoint as a carrier sees it.
pub trait TransportSide: Transport + CarrierSide {
    fn as_transport(&self) -> Arc<dyn Transport>;
    fn as_carrier_side(&self) -> Arc<dyn CarrierSide>;
}

pub type TransportTimerCancel = Arc<dyn Fn() + Send + Sync>;

/// The clock a transport schedules on; tests pass manual timers.
pub trait TransportClock: Send + Sync {
    fn now(&self) -> i64;
    /// Runs `callback` after `ms`; the returned function cancels it.
    fn set_timer(
        &self,
        callback: Box<dyn FnOnce() + Send + 'static>,
        ms: i64,
    ) -> TransportTimerCancel;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("transport closed")]
pub struct TransportClosedError;
