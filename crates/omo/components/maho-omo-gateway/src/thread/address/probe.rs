//! Single-device probe & pinned reply resolution (`thread/address/probe.ts`).

use super::super::address_core::{
    NotAskedReason, SessionRef, SilentReason, device_not_asked, devices_silent,
    no_thread_on_device, same_session,
};
use super::super::addressing::{ThreadAddressEntry, to_candidates};
use super::super::errors::thread_tool_failure;
use super::bare::listed_entry;
use super::refusals::{ThreadRefusal, as_refusal, refusal, tool_refusal};
use super::types::{
    Found, ProbeAnswer, RemoteAddressSurface, RemoteDeviceInfo, RemoteSessionRow, Resolution,
};

fn named_device<'a>(devices: &'a [RemoteDeviceInfo], id: &'a str) -> &'a str {
    devices
        .iter()
        .find(|d| d.device == id)
        .map(|d| d.label.as_str())
        .unwrap_or(id)
}

fn ended_row(device: &str, key: &str, thread: &str) -> RemoteSessionRow {
    RemoteSessionRow {
        device: device.to_owned(),
        durable_id: Some(thread.to_owned()),
        session_id: key.to_owned(),
        name: None,
        first_message: None,
        folder: String::new(),
        status: "resumable".to_owned(),
        created_at: String::new(),
        updated_at: None,
    }
}

pub async fn probe_across(
    remote: &dyn RemoteAddressSurface,
    thread: &str,
    roster: Option<&[RemoteDeviceInfo]>,
    here: Option<&str>,
) -> Option<Result<Found, ThreadRefusal>> {
    let owned_devices;
    let devices: &[RemoteDeviceInfo] = match roster {
        Some(r) => r,
        None => {
            owned_devices = remote.devices().await.ok()?;
            &owned_devices
        }
    };
    let asked: Vec<&RemoteDeviceInfo> = devices
        .iter()
        .filter(|d| d.online && here != Some(d.device.as_str()))
        .collect();

    let mut answers = Vec::with_capacity(asked.len());
    for d in &asked {
        let ans = remote.lookup(&d.device, thread).await;
        answers.push((d.device.clone(), ans));
    }
    if answers
        .iter()
        .any(|(_, a)| matches!(a, ProbeAnswer::Unavailable))
    {
        return None;
    }

    let holders: Vec<(&str, &str)> = answers
        .iter()
        .filter_map(|(dev, ans)| match ans {
            ProbeAnswer::Found { key } => Some((dev.as_str(), key.as_str())),
            _ => None,
        })
        .collect();
    if holders.len() == 1
        && let Some(&(dev, key)) = holders.first()
    {
        let entry = listed_entry(&ended_row(dev, key, thread), None);
        return Some(Ok(Found {
            entry,
            resolution: Resolution::Probe,
            all_scope: Some(true),
        }));
    }
    if holders.len() > 1 {
        let entries: Vec<ThreadAddressEntry> = holders
            .iter()
            .map(|(dev, key)| listed_entry(&ended_row(dev, key, thread), None))
            .collect();
        let candidates = to_candidates(&entries);
        let failure = thread_tool_failure(
            "ambiguous_target",
            format!("Session {thread} is held on more than one device; none was picked."),
            "Address the session on one device with <device>/<session id>.",
            Some(serde_json::json!({ "candidates": candidates })),
        );
        return Some(Err(ThreadRefusal {
            code: failure.code,
            message: failure.message,
            next_action: failure.next_action,
            details: failure.details,
            candidates: Some(candidates),
        }));
    }

    let silent: Vec<(&str, &str, Option<&str>)> = answers
        .iter()
        .filter_map(|(dev, ans)| match ans {
            ProbeAnswer::Silent { reason } => Some((
                dev.as_str(),
                named_device(devices, dev),
                Some(reason.as_str()),
            )),
            _ => None,
        })
        .collect();
    let mut answered = Vec::new();
    if let Some(h) = here {
        answered.push(h.to_owned());
    }
    for (dev, ans) in &answers {
        if !matches!(ans, ProbeAnswer::Silent { .. }) {
            answered.push(dev.clone());
        }
    }
    if !silent.is_empty() {
        return Some(Err(tool_refusal(
            &devices_silent(SilentReason::DeviceUnanswered, &silent),
            Some(thread),
            Some(&answered),
        )));
    }
    Some(Err(refusal(
        "not_found",
        format!("No online device of this account holds session {thread}."),
        "The session may live on a device that is offline: retry when that device is online, or call thread_list (tool.thread_list() in an eval cell).",
        Some(serde_json::json!({ "kind": "thread", "thread": thread })),
    )))
}

pub async fn pinned(
    remote: Option<&dyn RemoteAddressSurface>,
    pin_device: &str,
    pin_thread: &str,
    known: Option<(&[RemoteSessionRow], &[String], &[RemoteDeviceInfo])>,
) -> Result<Found, ThreadRefusal> {
    let Some(surface) = remote else {
        return Err(tool_refusal(
            &device_not_asked(
                NotAskedReason::RemoteUnavailable,
                pin_thread,
                pin_device,
                pin_device,
            ),
            None,
            None,
        ));
    };

    let device_label_name = match known {
        Some((_, _, roster)) => named_device(roster, pin_device).to_owned(),
        None => surface
            .devices()
            .await
            .map(|r| named_device(&r, pin_device).to_owned())
            .unwrap_or_else(|_| pin_device.to_owned()),
    };

    let check_listed = |rows: &[RemoteSessionRow],
                        incomplete: &[String]|
     -> Option<Result<Found, ThreadRefusal>> {
        if let Some(row) = rows.iter().find(|candidate| {
            same_session(
                &SessionRef {
                    device: pin_device,
                    thread: Some(pin_thread),
                    key: None,
                },
                &SessionRef {
                    device: &candidate.device,
                    thread: candidate.durable_id.as_deref(),
                    key: Some(&candidate.session_id),
                },
            )
        }) {
            return Some(Ok(Found {
                entry: listed_entry(row, None),
                resolution: Resolution::Id,
                all_scope: Some(true),
            }));
        }
        if incomplete.iter().any(|d| d == pin_device) {
            return Some(Err(tool_refusal(
                &devices_silent(
                    SilentReason::ListingIncomplete,
                    &[(pin_device, device_label_name.as_str(), None)],
                ),
                Some(pin_thread),
                None,
            )));
        }
        None
    };

    if let Some((rows, incomplete, _)) = known {
        if let Some(res) = check_listed(rows, incomplete) {
            return res;
        }
    } else {
        match surface.list_sessions(Some(pin_device)).await {
            Ok(listing) => {
                if let Some(res) = check_listed(&listing.rows, &listing.incomplete) {
                    return res;
                }
            }
            Err(reason) => {
                return Err(as_refusal(thread_tool_failure(
                    "host_unavailable",
                    "The account's session listing could not be reached.",
                    "Retry when the device service is reachable.",
                    Some(serde_json::json!({ "reason": reason })),
                )));
            }
        }
    }

    match surface.lookup(pin_device, pin_thread).await {
        ProbeAnswer::Found { key } => Ok(Found {
            entry: listed_entry(&ended_row(pin_device, &key, pin_thread), None),
            resolution: Resolution::Probe,
            all_scope: Some(true),
        }),
        ProbeAnswer::Silent { reason } => Err(tool_refusal(
            &devices_silent(
                SilentReason::DeviceUnanswered,
                &[(
                    pin_device,
                    device_label_name.as_str(),
                    Some(reason.as_str()),
                )],
            ),
            Some(pin_thread),
            None,
        )),
        ProbeAnswer::Unavailable => Err(tool_refusal(
            &device_not_asked(
                NotAskedReason::LookupUnavailable,
                pin_thread,
                pin_device,
                &device_label_name,
            ),
            None,
            None,
        )),
        ProbeAnswer::None => {
            if let Ok(listing) = surface.list_sessions(Some(pin_device)).await
                && let Some(res) = check_listed(&listing.rows, &listing.incomplete)
            {
                return res;
            }
            Err(tool_refusal(
                &no_thread_on_device(pin_thread, pin_device, &device_label_name),
                None,
                None,
            ))
        }
    }
}
