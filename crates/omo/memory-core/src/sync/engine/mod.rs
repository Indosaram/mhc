//! The device sync engine (sync-design §5-§8): journaled receive/apply/push cycles over ports.
#![deny(clippy::unwrap_used)]

pub mod adapters;
pub mod apply;
pub mod atomic;
pub mod batches;
#[allow(clippy::module_inception)] // mirrors the TS sync/engine/engine.ts layout
pub mod engine;
pub mod engine_deps;
pub mod error;
pub mod failure;
pub mod journal;
pub mod legacy_cycle;
pub mod local_signal;
pub mod open;
pub mod ports;
pub mod push;
pub mod receive;

pub use crate::sync::item::case_fold;
pub use adapters::http_transport::{HttpTransportOptions, SyncHttpError, create_http_transport};
pub use engine::{EngineStatus, SyncEngine};
pub use open::{DeviceSync, OpenDeviceSyncOptions, SyncSession, open_device_sync};
pub use ports::{RefusalCode, Sealer, Transport, Verifier};

pub use crate::sync::account::{
    AccountSessionIdle, AccountSessionOptions, AccountSessionOutcome, device_keyring_from_material,
    open_account_session,
};
pub use crate::sync::keyring_port::DeviceKeyMaterial;
pub use crate::sync::pin_dir::sync_pin_dir;
pub use failure::{SyncFailure, SyncFailureKind, describe_sync_failure, is_failure_shown};
