#![cfg(target_os = "macos")]

//! macOS desktop backend: Quartz capture, CGEvent input with SkyLight
//! background delivery and the focus guard, the AXUIElement accessibility
//! backend, and the `CGEventTap` kill switch (the `Global` stop path).

mod ax;
mod backend;
mod capture;
mod cursor;
mod focus;
mod front_app;
mod input;
mod launch_services;
mod release_helper;
mod responsible;
mod skylight;
mod stop_path;

pub use ax::{MacAx, is_trusted};
pub use backend::MacosBackend;
pub use input::{CanaryMode, CanaryResult};
pub use stop_path::{CgEventTapListener, DEFAULT_STOP_CHORD};

pub const BACKEND_NAME: &str = env!("CARGO_PKG_NAME");

/// The input-release helper this engine executable starts for an input
/// request (`--release-helper`); refuses to run unless that engine spawned it.
///
/// # Errors
/// The refusal: not started by the engine whose pid and token its argv name.
pub fn run_release_helper() -> Result<(), String> {
    release_helper::run()
}
