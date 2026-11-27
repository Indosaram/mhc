//! Port of `packages/coding-agent/src/engine-trust/*` (omo#10762, senpi#6312, senpi#5244).

pub mod actions;
pub mod adopted;
pub mod environment_names;
pub mod hand_off;
pub mod launch_environment;
pub mod marker;
pub mod shell_trust;
pub mod transport_failure;

pub use actions::{
    EngineTrustAction, EngineTrustActionId, EngineTrustActionReason, engine_trust_action,
};
pub use adopted::{adopted_engine_trust, record_adopted_engine_trust};
pub use hand_off::{adopted_engine_trust_hand_off, engine_trust_hand_off_environment};
pub use launch_environment::{
    TrustArrival, TrustLaunchOptions, TrustMarkerArrival, apply_engine_trust_restore,
    build_trust_launch_environment, classify_trust_arrival,
};
pub use marker::{
    ENGINE_TRUST_HOP_ENV, ENGINE_TRUST_MARKER_ENV, ENGINE_TRUST_RESTORE_NAMES,
    EngineTrustAutomatic, EngineTrustContractError, EngineTrustContractErrorCode,
    EngineTrustDefaults, EngineTrustFallbackReason, EngineTrustMarker, EngineTrustOwner,
    EngineTrustRestore, parse_engine_trust_marker, serialize_engine_trust_marker,
};
