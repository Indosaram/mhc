//! Top-level thread address resolver (`thread/address/resolve.ts`).

use std::collections::HashMap;

use super::super::address_core::{
    DeviceListing, DeviceRef, HERE, ParsedAddress, format_address, id_shaped, parse_address,
};
use super::bare::{AccountSnapshot, device_name_of, resolve_bare};
use super::device::resolve_account_device;
use super::on_device::resolve_on_device;
use super::probe::pinned;
use super::refusals::{ThreadRefusal, remote_unavailable_refusal, with_addresses};
use super::types::{
    Found, RemoteDeviceInfo, ResolveContext, ResolveNeed, ResolveOptions, ResolveWhere, Resolved,
};

fn ref_of(row: Option<&RemoteDeviceInfo>, id: &str, here: Option<&str>) -> DeviceRef {
    let online = row.map(|r| r.online).unwrap_or(true);
    DeviceRef {
        id: id.to_owned(),
        name: row
            .map(|r| r.label.clone())
            .unwrap_or_else(|| id.to_owned()),
        online,
        here: here == Some(id),
        listing: if online {
            DeviceListing::Listed
        } else {
            DeviceListing::Offline
        },
    }
}

fn finish(hit: Found, account: Option<&AccountSnapshot>) -> Resolved {
    let thread_id = hit.entry.summary.thread_id.clone();
    if !hit.entry.is_remote() {
        let here = account.and_then(|a| a.here.as_deref());
        let device = here.map(|h| {
            let row = account.and_then(|a| a.devices.iter().find(|d| d.device == h));
            ref_of(row, h, here)
        });
        let address = match &device {
            None => thread_id.clone(),
            Some(d) => format_address(&d.id, &thread_id),
        };
        return Resolved {
            thread: thread_id,
            address,
            device,
            where_located: ResolveWhere::Here {
                entry: hit.entry.clone(),
            },
            entry: hit.entry,
            resolution: hit.resolution,
            all_scope: hit.all_scope,
        };
    }

    let dev_id = hit.entry.device.clone().unwrap_or_default();
    let row_key = hit.entry.remote_row_key.clone().unwrap_or_default();
    let addr_tail = if thread_id.is_empty() {
        &row_key
    } else {
        &thread_id
    };
    let address = format_address(&dev_id, addr_tail);
    let here = account.and_then(|a| a.here.as_deref());
    let row = account.and_then(|a| a.devices.iter().find(|d| d.device == dev_id));
    let device = Some(ref_of(row, &dev_id, here));
    let synthetic_row = super::types::RemoteSessionRow {
        device: dev_id,
        durable_id: (!thread_id.is_empty()).then_some(thread_id.clone()),
        session_id: row_key,
        name: Some(hit.entry.summary.name.clone()),
        first_message: hit.entry.preview.clone(),
        folder: hit.entry.summary.cwd.clone(),
        status: if hit.entry.summary.status == super::super::contracts_results::ThreadStatus::Live {
            "live".to_owned()
        } else {
            "resumable".to_owned()
        },
        created_at: hit.entry.summary.created_at.clone(),
        updated_at: hit.entry.summary.updated_at.clone(),
    };
    Resolved {
        thread: thread_id,
        address,
        device,
        where_located: ResolveWhere::Device { row: synthetic_row },
        entry: hit.entry,
        resolution: hit.resolution,
        all_scope: hit.all_scope,
    }
}

async fn resolve_bare_resolved(
    book: &ResolveContext,
    address: &str,
    opts: &ResolveOptions,
) -> Result<Resolved, ThreadRefusal> {
    let (res, acct) = resolve_bare(book, address, opts).await;
    let hit = res?;
    Ok(finish(hit, acct.as_ref()))
}

/// The one resolver: `self`, bare, or `<device>/<thread>`.
///
/// In `O01` local-only mode (`ctx.remote == None`), `here/<thread>` resolves against this device,
/// and `<device>/<thread>` tries a local bare name match first (in case a local thread name contains
/// `/`) and otherwise returns a typed `remote_unavailable` refusal.
pub async fn resolve_address(
    ctx: &ResolveContext,
    text: &str,
    opts: &ResolveOptions,
) -> Result<Resolved, ThreadRefusal> {
    if let Some(ref pin) = opts.pin {
        let hit = pinned(ctx.remote.as_deref(), &pin.device, &pin.thread, None).await?;
        return Ok(finish(hit, None));
    }

    let parsed = parse_address(text);

    match parsed {
        ParsedAddress::SelfRef | ParsedAddress::Bare { .. } => {
            resolve_bare_resolved(ctx, text, opts).await
        }
        ParsedAddress::Device { device, thread, .. } => {
            let Some(remote) = ctx.remote.as_deref() else {
                if device == HERE {
                    let mut sub = ctx.clone();
                    sub.local = ctx.local_entries_for(&thread);
                    return resolve_bare_resolved(&sub, &thread, opts).await;
                }
                if let Ok(resolved) = resolve_bare_resolved(ctx, text, opts).await {
                    return Ok(resolved);
                }
                return Err(remote_unavailable_refusal(&device, Some(&thread)));
            };

            let devices = match remote.devices().await {
                Ok(d) => d,
                Err(_) => return resolve_bare_resolved(ctx, text, opts).await,
            };
            let here = remote.here();
            let dev_ref = match resolve_account_device(
                here.as_deref(),
                &devices,
                None,
                &device,
                opts.need == Some(ResolveNeed::Online),
                false,
            ) {
                Ok(d) => d,
                Err(dev_err) => {
                    return match resolve_bare_resolved(ctx, text, opts).await {
                        Ok(found) => Ok(found),
                        Err(_) => Err(dev_err),
                    };
                }
            };

            let listing = if dev_ref.here {
                super::types::RemoteListing {
                    rows: Vec::new(),
                    incomplete: Vec::new(),
                }
            } else {
                match remote.list_sessions(Some(&dev_ref.id)).await {
                    Ok(l) => l,
                    Err(_) => return resolve_bare_resolved(ctx, text, opts).await,
                }
            };
            let mut listings = HashMap::new();
            let dev_listing = if listing.incomplete.iter().any(|d| d == &dev_ref.id) {
                DeviceListing::Incomplete
            } else {
                DeviceListing::Listed
            };
            listings.insert(dev_ref.id.clone(), dev_listing);
            let mut refined_ref = dev_ref.clone();
            refined_ref.listing = dev_listing;

            let on_dev = resolve_on_device(
                ctx,
                remote,
                here.as_deref(),
                &devices,
                &listing.rows,
                &listing.incomplete,
                &refined_ref,
                &thread,
                opts,
            )
            .await;

            match on_dev {
                Ok(hit) => {
                    if !id_shaped(&thread) {
                        let mut local_only = ctx.clone();
                        local_only.remote = None;
                        if let (Ok(local_hit), _) = resolve_bare(&local_only, text, opts).await
                            && local_hit.entry.summary.thread_id != hit.entry.summary.thread_id
                        {
                            let candidates = vec![
                                super::super::addressing::to_candidate(&hit.entry),
                                super::super::addressing::to_candidate(&local_hit.entry),
                            ];
                            return Err(with_addresses(
                                super::super::addressing::ThreadAddressError {
                                    code: super::super::errors::ThreadErrorCode::parse("ambiguous_target")
                                        .unwrap_or_else(|| unreachable!()),
                                    message: format!(
                                        "\"{text}\" names a thread on a device and a thread named \"{text}\"; none was picked."
                                    ),
                                    next_action: "Pass the exact thread value of the intended thread from candidates.".to_owned(),
                                    details: None,
                                    candidates: Some(candidates),
                                },
                                here.as_deref(),
                                |id| device_name_of(&devices, id),
                            ));
                        }
                    }
                    let snap = AccountSnapshot {
                        here,
                        devices,
                        rows: listing.rows,
                        incomplete: listing.incomplete,
                    };
                    Ok(finish(hit, Some(&snap)))
                }
                Err(dev_err) => {
                    if id_shaped(&thread) {
                        return Err(dev_err);
                    }
                    match resolve_bare_resolved(ctx, text, opts).await {
                        Ok(whole) => Ok(whole),
                        Err(_) => Err(dev_err),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::addressing::ThreadAddressEntry;
    use super::super::super::contracts_results::{ThreadStatus, ThreadSummary};
    use super::*;

    fn entry(id: &str, name: &str, cwd: &str) -> ThreadAddressEntry {
        ThreadAddressEntry::from_summary(ThreadSummary {
            thread_id: id.into(),
            name: name.into(),
            status: ThreadStatus::Live,
            cwd: cwd.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: Some("2026-01-01T00:00:00Z".into()),
            error_note: None,
            endpoint: None,
            surface: None,
            alive: None,
            controls: None,
            skills: None,
            device: None,
            alias: None,
        })
    }

    #[tokio::test]
    async fn resolves_here_and_refuses_remote_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let ctx = ResolveContext::local_only(
            "t-self",
            &cwd,
            vec![
                entry("01965d50-0000-7000-8000-000000000001", "Self", &cwd),
                entry("01965d50-0000-7000-8000-000000000002", "Worker", &cwd),
            ],
        );
        let here = resolve_address(&ctx, "here/Worker", &ResolveOptions::default())
            .await
            .unwrap();
        assert_eq!(here.thread, "01965d50-0000-7000-8000-000000000002");

        let err = resolve_address(&ctx, "laptop/Worker", &ResolveOptions::default())
            .await
            .unwrap_err();
        assert_eq!(err.code.as_str(), "remote_unavailable");
    }
}
