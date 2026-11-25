//! Thread address resolution (`thread/address/*`).
//!
//! `O01` ports local resolution + typed refusals for cross-device addresses without pulling `O02`
//! (`device-service-core` / relay daemon clients).

pub mod bare;
pub mod device;
pub mod holder;
pub mod ladder;
pub mod on_device;
pub mod probe;
pub mod refusals;
pub mod resolve;
pub mod types;

pub use resolve::resolve_address;
pub use types::{
    Found, RemoteAddressSurface, RemoteDeviceInfo, RemoteSessionRow, Resolution, ResolveContext,
    ResolveOptions, ResolveWhere, Resolved, UNKNOWN_CALLER,
};
