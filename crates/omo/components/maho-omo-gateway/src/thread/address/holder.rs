//! Alias holder resolution (`thread/address/holder.ts`).

use super::super::errors::{ThreadToolFailure, thread_tool_failure};
use super::types::{LocateAnswer, RemoteAddressSurface, RemoteDeviceInfo, RemoteSessionRow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTarget {
    Remote { row: RemoteSessionRow },
    Here { session_id: String },
}

pub async fn holder_target(
    remote: &dyn RemoteAddressSurface,
    holder_device: &str,
    holder_session_id: &str,
    here: Option<&str>,
    rows: &[RemoteSessionRow],
    roster: Option<&[RemoteDeviceInfo]>,
) -> Result<RemoteTarget, ThreadToolFailure> {
    if here == Some(holder_device) {
        return Ok(RemoteTarget::Here {
            session_id: holder_session_id.to_owned(),
        });
    }
    if let Some(listed) = rows.iter().find(|row| {
        row.device == holder_device
            && (row.durable_id.as_deref() == Some(holder_session_id)
                || row.session_id == holder_session_id)
    }) {
        return Ok(RemoteTarget::Remote {
            row: listed.clone(),
        });
    }

    let owned_devices;
    let devices: &[RemoteDeviceInfo] = match roster {
        Some(r) => r,
        None => {
            owned_devices = remote.devices().await.map_err(|reason| {
                thread_tool_failure(
                    "host_unavailable",
                    "This device's device service could not list the account's devices.",
                    "Retry when the device service is reachable.",
                    Some(serde_json::json!({ "device": holder_device, "reason": reason })),
                )
            })?;
            &owned_devices
        }
    };
    let device = devices.iter().find(|d| d.device == holder_device);
    if roster.is_some() && device.is_none() {
        return Err(thread_tool_failure(
            "not_found",
            format!(
                "The alias names thread {holder_session_id} on device {holder_device}, which is no longer on this account."
            ),
            "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to find the live thread, then claim the alias for it again with thread_rename (tool.thread_rename()).",
            Some(serde_json::json!({ "device": holder_device, "thread": holder_session_id })),
        ));
    }
    if let Some(d) = device
        && !d.online
    {
        return Err(thread_tool_failure(
            "device_offline",
            format!("Device {holder_device} holding this thread is offline."),
            "Retry when that device is online; nothing was queued.",
            Some(serde_json::json!({ "device": holder_device, "thread": holder_session_id })),
        ));
    }

    let located = remote.locate(holder_device, holder_session_id).await;
    if let LocateAnswer::Ok { ref session_id } = located
        && let Some(row) = rows
            .iter()
            .find(|c| c.device == holder_device && &c.session_id == session_id)
    {
        return Ok(RemoteTarget::Remote { row: row.clone() });
    }
    let details = match located {
        LocateAnswer::Refused { reason } => {
            serde_json::json!({ "device": holder_device, "thread": holder_session_id, "reason": reason })
        }
        LocateAnswer::Ok { .. } => {
            serde_json::json!({ "device": holder_device, "thread": holder_session_id })
        }
    };
    Err(thread_tool_failure(
        "not_found",
        format!(
            "The alias names thread {holder_session_id} on device {holder_device}, which that device does not list."
        ),
        "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to find the live thread, then claim the alias for it again with thread_rename (tool.thread_rename() in an eval cell).",
        Some(details),
    ))
}
