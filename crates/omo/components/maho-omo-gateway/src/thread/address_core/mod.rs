//! Local & cross-device address core (`packages/thread-address-core/src/index.ts`).

pub mod device;
pub mod listing;
pub mod name;
pub mod parse;
pub mod refusals;
pub mod row;

pub use device::{DeviceRef, DeviceResolution, DeviceRoster, ResolveDeviceOptions, resolve_device};
pub use listing::{DeviceListing, ListingRound, device_listing};
pub use name::normalize_name;
pub use parse::{
    DEVICE_PREFIX_MIN, HERE, ID_PREFIX_SHAPE, ParsedAddress, SESSION_ID_SHAPE, THREAD_ID_SHAPE,
    format_address, id_shaped, is_id_prefix_shape, is_session_id_shape, is_thread_id_shape,
    parse_address,
};
pub use refusals::{
    AddressRefusal, DeviceCandidate, DeviceRow, NotAskedReason, RefusalCode, SilentReason,
    device_label, device_not_asked, device_offline, devices_share, devices_silent, no_device,
    no_thread_on_device,
};
pub use row::{
    KeyedRow, NamedRow, RowName, SessionRef, listing_key, merge_rows, row_address, row_key,
    row_name, same_session,
};
