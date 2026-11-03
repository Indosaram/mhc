//! Awaiting a large future without copying it through the caller's stack frame.

use std::future::Future;
use std::pin::Pin;

/// Builds the future `make` returns directly in a fresh heap allocation.
///
/// `Box::pin(self.run())` first materializes the whole `run()` state machine as a temporary in the
/// caller and then moves it into the box. Unoptimized builds give every such temporary its own slot
/// in the caller's poll frame (no stack-slot reuse), so an async fn that awaits several large
/// futures gets a frame as large as all of them combined, whichever branch actually runs; nested a
/// few levels deep, those frames overflow a 2 MiB thread stack on Windows. Calling `make` here keeps
/// the temporary in this short-lived frame, and the caller holds only the box pointer.
#[inline(never)]
pub fn heap_future<Fut: Future>(make: impl FnOnce() -> Fut) -> Pin<Box<Fut>> {
    Box::pin(make())
}
