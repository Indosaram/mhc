//! Typed address resolution refusals (`thread/address/refusals.ts`).

use serde_json::{Map, Value};

use super::super::address_core::{
    AddressRefusal, NotAskedReason, SilentReason, device_label, format_address,
};
use super::super::addressing::{ThreadAddressError, ThreadCandidate};
use super::super::errors::{ThreadToolFailure, thread_tool_failure};

pub type ThreadRefusal = ThreadAddressError;

pub fn refusal(
    code: &str,
    message: impl Into<String>,
    next_action: impl Into<String>,
    details: Option<Value>,
) -> ThreadRefusal {
    let failure = thread_tool_failure(code, message, next_action, details);
    ThreadRefusal {
        code: failure.code,
        message: failure.message,
        next_action: failure.next_action,
        details: failure.details,
        candidates: None,
    }
}

pub fn as_refusal(failure: ThreadToolFailure) -> ThreadRefusal {
    ThreadRefusal {
        code: failure.code,
        message: failure.message,
        next_action: failure.next_action,
        details: failure.details,
        candidates: None,
    }
}

pub fn as_failure(refused: ThreadRefusal) -> ThreadToolFailure {
    refused.into_failure()
}

pub fn remote_unavailable_refusal(device: &str, thread: Option<&str>) -> ThreadRefusal {
    let mut details = Map::new();
    details.insert(
        "reason".to_owned(),
        Value::String("remote_unavailable".to_owned()),
    );
    details.insert("device".to_owned(), Value::String(device.to_owned()));
    if let Some(t) = thread {
        details.insert("thread".to_owned(), Value::String(t.to_owned()));
    }
    refusal(
        "remote_unavailable",
        format!("Cross-device addressing for \"{device}\" is unavailable in this local session."),
        "Target a local thread (use tool.thread_list() in an eval cell) or use \"here/<thread>\".",
        Some(Value::Object(details)),
    )
}

pub fn account_unavailable_refusal(reason: &str) -> ThreadRefusal {
    refusal(
        "account_unavailable",
        format!("No signed-in account is available to resolve cross-device addresses ({reason})."),
        "Target a local thread on this device.",
        Some(serde_json::json!({ "reason": reason })),
    )
}

pub fn device_not_found_refusal(typed: &str) -> ThreadRefusal {
    refusal(
        "device_not_found",
        format!("No device named \"{typed}\" is known."),
        "Pass \"here\" for this device or list local threads with tool.thread_list().",
        Some(serde_json::json!({ "kind": "device", "device": typed })),
    )
}

const LIST_ALL: &str =
    "call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell)";
const RETRY_LIST: &str = "Retry the call; call thread_list (tool.thread_list() in an eval cell) to see which devices answered.";

fn labels(ids: &[String], names: &[String]) -> String {
    ids.iter()
        .enumerate()
        .map(|(i, id)| device_label(id, names.get(i).map(String::as_str).unwrap_or("")))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn tool_refusal(
    core: &AddressRefusal,
    thread: Option<&str>,
    answered: Option<&[String]>,
) -> ThreadRefusal {
    match core {
        AddressRefusal::NoDevice { typed, candidates } => refusal(
            "not_found",
            format!("No device named {typed} is signed in to this account."),
            format!("Pass a device name or id from candidates, or {LIST_ALL}."),
            Some(
                serde_json::json!({ "kind": "device", "device": typed, "candidates": candidates }),
            ),
        ),
        AddressRefusal::DevicesShare {
            by,
            typed,
            candidates,
        } => {
            let msg = match by.as_str() {
                "devices_share_name" => {
                    let online_word = if candidates.iter().all(|c| c.online) {
                        "online "
                    } else {
                        ""
                    };
                    format!(
                        "{} {online_word}devices are named {typed}; none was picked.",
                        candidates.len()
                    )
                }
                _ => format!(
                    "{} devices have an id starting {typed}; none was picked.",
                    candidates.len()
                ),
            };
            let next = if by.as_str() == "devices_share_name" {
                "Pass the device id from candidates."
            } else {
                "Pass more of the device id, or its name."
            };
            refusal(
                "ambiguous_target",
                msg,
                next,
                Some(
                    serde_json::json!({ "kind": "device", "device": typed, "candidates": candidates }),
                ),
            )
        }
        AddressRefusal::DeviceOffline {
            typed,
            device,
            device_name,
            last_seen,
            candidates,
        } => refusal(
            "device_offline",
            format!("Device {typed} is offline."),
            "Retry when that device is online; nothing was queued.",
            Some(serde_json::json!({
                "device": device,
                "device_name": device_name,
                "last_seen": last_seen,
                "candidates": candidates,
            })),
        ),
        AddressRefusal::DevicesSilent {
            reason,
            devices,
            device_names,
            device_reasons,
        } => match reason {
            SilentReason::ListingIncomplete => refusal(
                "host_unavailable",
                format!(
                    "Nothing matched on the devices that answered; {} did not answer this listing round.",
                    labels(devices, device_names)
                ),
                RETRY_LIST,
                Some(serde_json::json!({
                    "reason": "listing_incomplete",
                    "devices": devices,
                    "device_names": device_names,
                })),
            ),
            SilentReason::DeviceUnanswered => {
                let mut slow = Vec::new();
                let mut unanswered = Vec::new();
                for (idx, id) in devices.iter().enumerate() {
                    let lbl =
                        device_label(id, device_names.get(idx).map(String::as_str).unwrap_or(""));
                    if device_reasons.get(id).map(String::as_str) == Some("lookup_timeout") {
                        slow.push(lbl);
                    } else {
                        unanswered.push(lbl);
                    }
                }
                let mut outcomes = Vec::new();
                if !slow.is_empty() {
                    outcomes.push(format!(
                        "{} {} too slow for the lookup",
                        slow.join(", "),
                        if slow.len() == 1 { "was" } else { "were" }
                    ));
                }
                if !unanswered.is_empty() {
                    outcomes.push(format!("{} did not answer", unanswered.join(", ")));
                }
                let retry = if slow.is_empty() {
                    format!(
                        "Retry when {} {}.",
                        unanswered.join(", "),
                        if unanswered.len() == 1 {
                            "answers"
                        } else {
                            "answer"
                        }
                    )
                } else {
                    "Retry the lookup; a slow device may answer in time on another attempt."
                        .to_owned()
                };
                let next = match answered {
                    Some(a) if !a.is_empty() => format!(
                        "{retry} If the session should be on a device that answered, its file is not where that device looks for sessions."
                    ),
                    _ => retry,
                };
                let answered_str = match answered {
                    Some(a) if !a.is_empty() => format!(" ({})", a.join(", ")),
                    _ => String::new(),
                };
                let target_desc = match thread {
                    Some(t) => format!("session {t}"),
                    None => "it".to_owned(),
                };
                let mut details = serde_json::json!({
                    "reason": "device_unanswered",
                    "devices": devices,
                    "device_names": device_names,
                    "device_reasons": device_reasons,
                });
                if let Some(obj) = details.as_object_mut() {
                    if let Some(t) = thread {
                        obj.insert("thread".to_owned(), Value::String(t.to_owned()));
                    }
                    if let Some(a) = answered {
                        obj.insert(
                            "answered".to_owned(),
                            Value::Array(a.iter().map(|s| Value::String(s.clone())).collect()),
                        );
                    }
                }
                refusal(
                    "host_unavailable",
                    format!(
                        "No device that answered{answered_str} holds {target_desc}; {}.",
                        outcomes.join(", and ")
                    ),
                    next,
                    Some(details),
                )
            }
        },
        AddressRefusal::DeviceNotAsked {
            reason,
            thread: t,
            device,
            device_name,
        } => {
            let lbl = device_label(device, device_name);
            let details = Some(serde_json::json!({
                "reason": reason.as_str(),
                "thread": t,
                "device": device,
                "device_name": device_name,
            }));
            match reason {
                NotAskedReason::RemoteUnavailable => refusal(
                    "host_unavailable",
                    format!(
                        "This device has no connection to the account's other devices, so {lbl} could not be asked for {t}."
                    ),
                    "Retry from a session whose device service is set up, or send to another thread as a normal send.",
                    details,
                ),
                NotAskedReason::WakeUnavailable => refusal(
                    "host_unavailable",
                    format!(
                        "{lbl} does not list {t}, and this device's service cannot carry a wake, so it does not probe, so whether it still holds that session is unknown."
                    ),
                    "Retry when the device service is reachable, or send to another thread as a normal send.",
                    details,
                ),
                NotAskedReason::LookupUnavailable => refusal(
                    "host_unavailable",
                    format!(
                        "{lbl} does not list {t}, and this device's service cannot make the single probe that would tell, so whether it still holds that session is unknown."
                    ),
                    "Retry when the device service is reachable, or send to another thread as a normal send.",
                    details,
                ),
            }
        }
        AddressRefusal::NoThreadOnDevice {
            thread: t,
            device,
            device_name,
        } => refusal(
            "not_found",
            format!("No thread {t} on {}.", device_label(device, device_name)),
            format!("Pass a thread that device holds, or {LIST_ALL}."),
            Some(serde_json::json!({
                "kind": "thread",
                "thread": t,
                "device": device,
                "device_name": device_name,
            })),
        ),
    }
}

pub fn with_addresses(
    mut error: ThreadAddressError,
    here: Option<&str>,
    name_of: impl Fn(&str) -> Option<String>,
) -> ThreadRefusal {
    let Some(cands) = error.candidates.take() else {
        if matches!(error.code.as_str(), "not_found" | "ambiguous_target") {
            error.details = Some(serde_json::json!({ "kind": "thread" }));
        }
        return error;
    };
    let enriched: Vec<ThreadCandidate> = cands
        .into_iter()
        .map(|mut candidate| {
            let device = match candidate.device.as_deref() {
                Some("here") => Some(here.unwrap_or("here").to_owned()),
                other => other.map(str::to_owned),
            };
            let device_name = device.as_deref().and_then(&name_of);
            if let Some(ref dev) = device {
                let prefix = format!("{dev}/");
                candidate.address = Some(if candidate.thread.starts_with(&prefix) {
                    candidate.thread.clone()
                } else {
                    format_address(dev, &candidate.thread)
                });
            }
            candidate.device = device;
            candidate.device_name = device_name;
            candidate
        })
        .collect();
    error.details = Some(serde_json::json!({ "kind": "thread", "candidates": enriched }));
    error.candidates = Some(enriched);
    error
}
