//! Remote mirror synchronization and secret redaction.

pub mod account;
pub mod canonical_json;
pub mod canonical_json_serialize;
pub mod canonical_json_types;
pub mod crypto;
pub mod engine;
pub mod hpke_sealed_box;
pub mod item;
pub mod keyring_port;
pub mod mirror;
pub mod pin_dir;
pub mod recovery;
pub mod redact;
pub mod space;

pub use canonical_json::{
    CanonicalJsonError, CanonicalJsonValue, CanonicalJsonViolation, MAX_CANONICAL_DEPTH,
    canonicalize_json, require_canonical_json,
};
pub use keyring_port::{
    DeviceKeyMaterial, DeviceKeyring, DeviceKeyringError, DeviceKeyringImpl,
    DeviceKeyringSignError, DeviceKeyringSignRequest, DevicePrincipal, DevicePublicKeys,
    GeneratedDeviceKeyMaterial, HpkeSealedBoxError, WrapBinding, create_device_keyring,
    create_in_memory_device_keyring, generate_device_key_material, open_hpke_sealed_box,
};

pub use mirror::{
    CONFIG_KEY, LOG_NAME, MirrorPushResult, MirrorSetResult, MirrorStatus, MirrorSync,
    SYNC_PUSH_ENV, SyncError, get_post_commit_hook_script, mirror_log_path, normalize_url,
};
pub use redact::{
    SECRET_PATTERN_SOURCES, SecretMatch, SecretPatternClass, contains_secret_like_material,
    redact_secret_like_material, redact_url, scan_secret_like_material,
};

pub use pin_dir::sync_pin_dir;
