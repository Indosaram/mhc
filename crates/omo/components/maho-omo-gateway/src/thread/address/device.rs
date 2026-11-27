//! Device resolution for thread addressing (`thread/address/device.ts`).

use std::collections::HashMap;

use super::super::address_core::{
    DeviceListing, DeviceRef, DeviceResolution, DeviceRoster, DeviceRow, ResolveDeviceOptions,
    resolve_device,
};
use super::super::errors::{ThreadToolFailure, thread_tool_failure};
use super::refusals::{ThreadRefusal, as_failure, remote_unavailable_refusal, tool_refusal};
use super::types::{RemoteAddressSurface, RemoteDeviceInfo};

pub fn device_rows(devices: &[RemoteDeviceInfo]) -> Vec<DeviceRow> {
    devices
        .iter()
        .map(|row| DeviceRow {
            id: row.device.clone(),
            name: row.label.clone(),
            online: row.online,
            last_seen_ms: row.last_seen_ms,
        })
        .collect()
}

#[allow(clippy::result_large_err)]
pub fn resolve_account_device(
    here: Option<&str>,
    devices: &[RemoteDeviceInfo],
    listings: Option<&HashMap<String, DeviceListing>>,
    text: &str,
    online: bool,
    exact_id: bool,
) -> Result<DeviceRef, ThreadRefusal> {
    let roster = DeviceRoster {
        here: here.map(str::to_owned),
        devices: device_rows(devices),
        listings: listings.cloned().unwrap_or_default(),
    };
    match resolve_device(&roster, text, ResolveDeviceOptions { online, exact_id }) {
        DeviceResolution::Ok(d) => Ok(d),
        DeviceResolution::Refusal(r) => Err(tool_refusal(&r, None, None)),
    }
}

/// The device a `thread_create` names (`#14072`): `None` when it is this device, else its id.
pub async fn target_device(
    remote: Option<&dyn RemoteAddressSurface>,
    text: &str,
) -> Result<Option<String>, ThreadToolFailure> {
    if text == "here" {
        return Ok(None);
    }
    let Some(surface) = remote else {
        return Err(as_failure(remote_unavailable_refusal(text, None)));
    };
    let here = surface.here();
    let devices = surface.devices().await.map_err(|reason| {
        thread_tool_failure(
            "host_unavailable",
            "This device's device service could not list the account's devices.",
            "Retry when the device service is reachable.",
            Some(serde_json::json!({ "device": text, "reason": reason })),
        )
    })?;
    let resolved = resolve_account_device(here.as_deref(), &devices, None, text, true, false)
        .map_err(as_failure)?;
    if here.as_deref() == Some(resolved.id.as_str()) {
        Ok(None)
    } else {
        Ok(Some(resolved.id))
    }
}

pub async fn resolve_typed_device(
    remote: Option<&dyn RemoteAddressSurface>,
    typed: &str,
    exact_id: bool,
) -> Result<(String, Vec<RemoteDeviceInfo>), ThreadToolFailure> {
    let Some(surface) = remote else {
        return Err(as_failure(remote_unavailable_refusal(typed, None)));
    };
    let here = surface.here();
    let devices = surface.devices().await.map_err(|reason| {
        thread_tool_failure(
            "host_unavailable",
            "This device's device service could not list the account's devices.",
            "Retry when the device service is reachable.",
            Some(serde_json::json!({ "device": typed, "reason": reason })),
        )
    })?;
    let resolved = resolve_account_device(here.as_deref(), &devices, None, typed, true, exact_id)
        .map_err(as_failure)?;
    Ok((resolved.id, devices))
}
