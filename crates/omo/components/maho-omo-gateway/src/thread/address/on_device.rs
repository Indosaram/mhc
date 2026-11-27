//! Per-device thread resolution (`thread/address/on-device.ts`).

use super::super::address_core::{
    DeviceListing, DeviceRef, DeviceRow, SessionRef, SilentReason, device_offline, devices_silent,
    id_shaped, is_thread_id_shape, no_thread_on_device, same_session,
};
use super::bare::{device_name_of, listed_entry};
use super::device::device_rows;
use super::holder::{RemoteTarget, holder_target};
use super::ladder::{LadderResult, ladder_async};
use super::probe::pinned;
use super::refusals::{ThreadRefusal, tool_refusal, with_addresses};
use super::types::{
    Found, RemoteAddressSurface, RemoteDeviceInfo, RemoteSessionRow, Resolution, ResolveContext,
    ResolveOptions,
};

async fn alias_on(
    remote: &dyn RemoteAddressSurface,
    here: Option<&str>,
    rows: &[RemoteSessionRow],
    roster: &[RemoteDeviceInfo],
    device: &str,
    alias: &str,
) -> Option<Found> {
    let (holder_device, holder_session_id) = remote.resolve_alias(alias).await?;
    if holder_device != device {
        return None;
    }
    match holder_target(
        remote,
        &holder_device,
        &holder_session_id,
        here,
        rows,
        Some(roster),
    )
    .await
    {
        Ok(RemoteTarget::Remote { row }) => Some(Found {
            entry: listed_entry(&row, Some(&[alias.to_owned()])),
            resolution: Resolution::Alias,
            all_scope: Some(true),
        }),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn resolve_on_device(
    ctx: &ResolveContext,
    remote: &dyn RemoteAddressSurface,
    here: Option<&str>,
    devices: &[RemoteDeviceInfo],
    all_rows: &[RemoteSessionRow],
    incomplete: &[String],
    device: &DeviceRef,
    thread: &str,
    opts: &ResolveOptions,
) -> Result<Found, ThreadRefusal> {
    if device.here {
        let local = ctx.local_entries_for(thread);
        return match ladder_async(ctx, &local, thread, opts, None).await {
            LadderResult::Ok { entry, resolution } => Ok(Found {
                entry,
                resolution,
                all_scope: Some(opts.all_scope),
            }),
            LadderResult::Error(err) => {
                Err(with_addresses(err, here, |id| device_name_of(devices, id)))
            }
        };
    }

    let rows: Vec<RemoteSessionRow> = all_rows
        .iter()
        .filter(|r| r.device == device.id)
        .cloned()
        .collect();
    if let Some(listed) = rows.iter().find(|row| {
        same_session(
            &SessionRef {
                device: &device.id,
                thread: Some(thread),
                key: Some(thread),
            },
            &SessionRef {
                device: &row.device,
                thread: row.durable_id.as_deref(),
                key: Some(&row.session_id),
            },
        )
    }) {
        return Ok(Found {
            entry: listed_entry(listed, None),
            resolution: if is_thread_id_shape(thread) {
                Resolution::Id
            } else {
                Resolution::SessionId
            },
            all_scope: Some(true),
        });
    }

    let offline = || -> ThreadRefusal {
        let row = device_rows(devices)
            .into_iter()
            .find(|c| c.id == device.id)
            .unwrap_or(DeviceRow {
                id: device.id.clone(),
                name: device.name.clone(),
                online: false,
                last_seen_ms: None,
            });
        tool_refusal(&device_offline(&device.name, &row, &[]), None, None)
    };

    if is_thread_id_shape(thread) {
        if !device.online {
            return Err(offline());
        }
        return pinned(
            Some(remote),
            &device.id,
            thread,
            Some((&rows, incomplete, devices)),
        )
        .await;
    }

    let entries: Vec<_> = rows.iter().map(|r| listed_entry(r, None)).collect();
    let mut scoped_opts = opts.clone();
    scoped_opts.all_scope = true;
    match ladder_async(ctx, &entries, thread, &scoped_opts, None).await {
        LadderResult::Ok { entry, resolution } => {
            return Ok(Found {
                entry,
                resolution,
                all_scope: Some(true),
            });
        }
        LadderResult::Error(err) if err.code.as_str() == "ambiguous_target" => {
            return Err(with_addresses(err, here, |id| device_name_of(devices, id)));
        }
        LadderResult::Error(_) => {}
    }

    if let Some(found) = alias_on(remote, here, all_rows, devices, &device.id, thread).await {
        return Ok(found);
    }
    if !device.online {
        return Err(offline());
    }
    if id_shaped(thread) {
        let synthetic = RemoteSessionRow {
            device: device.id.clone(),
            durable_id: None,
            session_id: thread.to_owned(),
            name: None,
            first_message: None,
            folder: String::new(),
            status: "live".to_owned(),
            created_at: String::new(),
            updated_at: None,
        };
        return Ok(Found {
            entry: listed_entry(&synthetic, None),
            resolution: Resolution::SessionId,
            all_scope: Some(true),
        });
    }
    if device.listing == DeviceListing::Incomplete {
        return Err(tool_refusal(
            &devices_silent(
                SilentReason::ListingIncomplete,
                &[(&device.id, &device.name, None)],
            ),
            None,
            None,
        ));
    }
    Err(tool_refusal(
        &no_thread_on_device(thread, &device.id, &device.name),
        None,
        None,
    ))
}
