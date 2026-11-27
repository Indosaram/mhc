//! Bare address resolution (`thread/address/bare.ts`).

use super::super::address_core::{
    NamedRow, SilentReason, devices_silent, is_thread_id_shape, row_name,
};
use super::super::addressing::{ThreadAddressEntry, fuzzy_contenders};
use super::super::contracts_results::{ThreadStatus, ThreadSummary};
use super::super::errors::{ThreadToolFailure, thread_tool_failure};
use super::holder::{RemoteTarget, holder_target};
use super::ladder::{LadderResult, ladder_async};
use super::probe::probe_across;
use super::refusals::{ThreadRefusal, as_refusal, tool_refusal, with_addresses};
use super::types::{
    Found, RemoteAddressSurface, RemoteDeviceInfo, RemoteSessionRow, Resolution, ResolveContext,
    ResolveMatch, ResolveOptions,
};

pub fn device_name_of(devices: &[RemoteDeviceInfo], id: &str) -> Option<String> {
    devices
        .iter()
        .find(|d| d.device == id)
        .map(|d| d.label.clone())
}

pub fn listed_entry(row: &RemoteSessionRow, aliases: Option<&[String]>) -> ThreadAddressEntry {
    let thread_id = row.durable_id.clone().unwrap_or_default();
    let named = row_name(&NamedRow {
        name: row.name.as_deref(),
        first_message: row.first_message.as_deref(),
        folder: &row.folder,
    });
    let session_ids = if thread_id.is_empty() && !row.session_id.is_empty() {
        Some(vec![row.session_id.clone()])
    } else {
        None
    };
    ThreadAddressEntry {
        summary: ThreadSummary {
            thread_id,
            name: named.name,
            status: if row.status == "live" {
                ThreadStatus::Live
            } else {
                ThreadStatus::Resumable
            },
            cwd: row.folder.clone(),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
            error_note: None,
            endpoint: None,
            surface: None,
            alive: None,
            controls: None,
            skills: None,
            device: Some(row.device.clone()),
            alias: aliases.and_then(|a| a.first().cloned()),
        },
        preview: row.first_message.clone(),
        device: Some(row.device.clone()),
        aliases: aliases.map(|a| a.to_vec()),
        session_ids,
        remote_row_key: Some(row.session_id.clone()),
    }
}

pub struct AccountSnapshot {
    pub here: Option<String>,
    pub devices: Vec<RemoteDeviceInfo>,
    pub rows: Vec<RemoteSessionRow>,
    pub incomplete: Vec<String>,
}

pub async fn load_account_snapshot(
    remote: &dyn RemoteAddressSurface,
    device: Option<&str>,
) -> Result<AccountSnapshot, ThreadToolFailure> {
    let here = remote.here();
    let devices = remote.devices().await.map_err(|reason| {
        thread_tool_failure(
            "host_unavailable",
            "The account's session listing could not be reached.",
            "Retry when the device service is reachable.",
            Some(serde_json::json!({ "reason": reason })),
        )
    })?;
    let listing = remote.list_sessions(device).await.map_err(|reason| {
        thread_tool_failure(
            "host_unavailable",
            "The account's session listing could not be reached.",
            "Retry when the device service is reachable.",
            Some(serde_json::json!({ "reason": reason })),
        )
    })?;
    Ok(AccountSnapshot {
        here,
        devices,
        rows: listing.rows,
        incomplete: listing.incomplete,
    })
}

pub async fn resolve_bare(
    ctx: &ResolveContext,
    text: &str,
    opts: &ResolveOptions,
) -> (Result<Found, ThreadRefusal>, Option<AccountSnapshot>) {
    if opts.match_mode == Some(ResolveMatch::Fuzzy)
        && !opts.all_scope
        && ctx.caller_workspace_root.trim().is_empty()
    {
        return (
            Err(as_refusal(thread_tool_failure(
                "caller_context_missing",
                "Scoping a fuzzy handoff needs the caller's workspace root, which this call did not provide.",
                "Set all_scope to search every workspace, or hand off from a session that knows its workspace.",
                None,
            ))),
            None,
        );
    }
    let here_hit = ladder_async(ctx, &ctx.local, text, opts, None).await;
    let exact_here = matches!(
        here_hit,
        LadderResult::Ok {
            resolution: Resolution::SelfRef | Resolution::Id | Resolution::SessionId,
            ..
        }
    );
    let Some(remote) = ctx.remote.as_deref() else {
        return match here_hit {
            LadderResult::Ok { entry, resolution } => (
                Ok(Found {
                    entry,
                    resolution,
                    all_scope: Some(opts.all_scope),
                }),
                None,
            ),
            LadderResult::Error(err) => (Err(with_addresses(err, None, |_| None)), None),
        };
    };
    if exact_here && let LadderResult::Ok { entry, resolution } = here_hit {
        return (
            Ok(Found {
                entry,
                resolution,
                all_scope: Some(opts.all_scope),
            }),
            None,
        );
    }

    let account = match load_account_snapshot(remote, None).await {
        Ok(snap) => snap,
        Err(failure) => {
            if let LadderResult::Ok { entry, resolution } = here_hit {
                return (
                    Ok(Found {
                        entry,
                        resolution,
                        all_scope: Some(opts.all_scope),
                    }),
                    None,
                );
            }
            return (Err(as_refusal(failure)), None);
        }
    };

    let mut remote_entries: Vec<ThreadAddressEntry> = account
        .rows
        .iter()
        .filter(|r| account.here.as_deref() != Some(r.device.as_str()))
        .map(|r| listed_entry(r, None))
        .collect();
    let mut alias_failure: Option<ThreadToolFailure> = None;
    if let Some((holder_dev, holder_sid)) = remote.resolve_alias(text).await {
        match holder_target(
            remote,
            &holder_dev,
            &holder_sid,
            account.here.as_deref(),
            &account.rows,
            Some(&account.devices),
        )
        .await
        {
            Ok(RemoteTarget::Here { session_id }) => {
                remote_entries.push(ThreadAddressEntry {
                    summary: ThreadSummary {
                        thread_id: session_id,
                        name: text.to_owned(),
                        status: ThreadStatus::Live,
                        cwd: String::new(),
                        created_at: String::new(),
                        updated_at: None,
                        error_note: None,
                        endpoint: None,
                        surface: None,
                        alive: None,
                        controls: None,
                        skills: None,
                        device: None,
                        alias: Some(text.to_owned()),
                    },
                    preview: None,
                    device: None,
                    aliases: Some(vec![text.to_owned()]),
                    session_ids: None,
                    remote_row_key: None,
                });
            }
            Ok(RemoteTarget::Remote { row }) => {
                remote_entries
                    .retain(|e| e.remote_row_key.as_deref() != Some(row.session_id.as_str()));
                remote_entries.push(listed_entry(&row, Some(&[text.to_owned()])));
            }
            Err(err) => alias_failure = Some(err),
        }
    }

    if remote_entries.is_empty()
        && let LadderResult::Ok { entry, resolution } = here_hit
    {
        return (
            Ok(Found {
                entry,
                resolution,
                all_scope: Some(opts.all_scope),
            }),
            Some(account),
        );
    }

    let mut all = ctx.local.clone();
    all.extend(remote_entries.iter().cloned());
    let ranked = if opts.match_mode == Some(ResolveMatch::Fuzzy) {
        let mut r = ctx.local.clone();
        let mut resumable = Vec::new();
        for e in &remote_entries {
            if e.summary.status == ThreadStatus::Live {
                r.push(e.clone());
            } else {
                resumable.push(e.clone());
            }
        }
        r.extend(fuzzy_contenders(&resumable, text));
        Some(r)
    } else {
        None
    };

    match ladder_async(ctx, &all, text, opts, ranked.as_deref()).await {
        LadderResult::Ok { entry, resolution } => {
            let all_scope = if resolution == Resolution::Alias || entry.is_remote() {
                Some(true)
            } else {
                Some(opts.all_scope)
            };
            (
                Ok(Found {
                    entry,
                    resolution,
                    all_scope,
                }),
                Some(account),
            )
        }
        LadderResult::Error(across_err) => {
            if let Some(af) = alias_failure {
                return (Err(as_refusal(af)), Some(account));
            }
            if opts.probe
                && across_err.code.as_str() == "not_found"
                && is_thread_id_shape(text)
                && let Some(looked) = probe_across(
                    remote,
                    text,
                    Some(&account.devices),
                    account.here.as_deref(),
                )
                .await
            {
                return (looked, Some(account));
            }
            let missed_here =
                matches!(&here_hit, LadderResult::Error(e) if e.code.as_str() == "not_found");
            if missed_here
                && across_err.code.as_str() == "not_found"
                && !account.incomplete.is_empty()
            {
                let silent: Vec<(&str, String, Option<&str>)> = account
                    .incomplete
                    .iter()
                    .filter(|d| account.here.as_deref() != Some(d.as_str()))
                    .map(|d| {
                        let name = device_name_of(&account.devices, d).unwrap_or_else(|| d.clone());
                        (d.as_str(), name, None)
                    })
                    .collect();
                if !silent.is_empty() {
                    let borrowed: Vec<(&str, &str, Option<&str>)> = silent
                        .iter()
                        .map(|(id, name, r)| (*id, name.as_str(), *r))
                        .collect();
                    return (
                        Err(tool_refusal(
                            &devices_silent(SilentReason::ListingIncomplete, &borrowed),
                            None,
                            None,
                        )),
                        Some(account),
                    );
                }
            }
            (
                Err(with_addresses(across_err, account.here.as_deref(), |id| {
                    device_name_of(&account.devices, id)
                })),
                Some(account),
            )
        }
    }
}
