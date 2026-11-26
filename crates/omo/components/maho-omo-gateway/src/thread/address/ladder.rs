//! Ladder resolution (`self`, exact id, session id, alias, name, id_prefix, fuzzy) (`thread/address/ladder.ts`).

use super::super::addressing::{
    IdPrefixMode, ThreadAddressEntry, ThreadAddressError, ThreadFuzzyResult, ThreadResolveOptions,
    ThreadResolveResult, ThreadTargetResolution, fuzzy_match, resolve_target, workspace_entries,
};
use super::super::errors::thread_tool_failure;
use super::types::{Resolution, ResolveContext, ResolveMatch, ResolveOptions, UNKNOWN_CALLER};

#[allow(clippy::large_enum_variant)]
pub enum LadderResult {
    Ok {
        entry: ThreadAddressEntry,
        resolution: Resolution,
    },
    Error(ThreadAddressError),
}

fn named(text: &str, entry: ThreadAddressEntry, res: ThreadTargetResolution) -> LadderResult {
    let resolution = if text == "self" {
        Resolution::SelfRef
    } else if res == ThreadTargetResolution::Id && entry.summary.thread_id != text {
        Resolution::SessionId
    } else {
        match res {
            ThreadTargetResolution::Id => Resolution::Id,
            ThreadTargetResolution::Alias => Resolution::Alias,
            ThreadTargetResolution::Name => Resolution::Name,
            ThreadTargetResolution::IdPrefix => Resolution::IdPrefix,
        }
    };
    LadderResult::Ok { entry, resolution }
}

fn exact_ladder(
    ctx: &ResolveContext,
    entries: &[ThreadAddressEntry],
    text: &str,
    opts: &ResolveOptions,
) -> LadderResult {
    let target: String = if text == "self" {
        let caller = if ctx.caller_id == UNKNOWN_CALLER {
            None
        } else {
            entries
                .iter()
                .find(|e| e.summary.thread_id == ctx.caller_id)
                .or_else(|| {
                    entries.iter().find(|e| {
                        e.session_ids
                            .as_ref()
                            .is_some_and(|ids| ids.iter().any(|id| id == &ctx.caller_id))
                    })
                })
        };
        let Some(caller_entry) = caller else {
            let f = thread_tool_failure(
                "caller_context_missing",
                "The caller's durable session id is not in the thread address book.",
                "Call thread_list (tool.thread_list() in an eval cell) and pass an explicit thread, or retry from a session with caller context.",
                None,
            );
            return LadderResult::Error(ThreadAddressError {
                code: f.code,
                message: f.message,
                next_action: f.next_action,
                details: f.details,
                candidates: None,
            });
        };
        caller_entry.summary.thread_id.clone()
    } else {
        text.to_owned()
    };

    let res = resolve_target(
        entries,
        &target,
        &ThreadResolveOptions {
            all_scope: opts.all_scope,
            caller_workspace_root: Some(ctx.caller_workspace_root.clone()),
            id_prefix: opts.id_prefix.clone(),
        },
    );
    match res {
        ThreadResolveResult::Ok { entry, resolution } => named(text, entry, resolution),
        ThreadResolveResult::Error(err) => LadderResult::Error(err),
    }
}

pub fn ladder(
    ctx: &ResolveContext,
    entries: &[ThreadAddressEntry],
    text: &str,
    opts: &ResolveOptions,
    ranked: Option<&[ThreadAddressEntry]>,
) -> LadderResult {
    let exact = exact_ladder(ctx, entries, text, opts);
    if opts.match_mode != Some(ResolveMatch::Fuzzy) {
        return exact;
    }
    if let LadderResult::Ok { ref entry, .. } = exact
        && entry.summary.thread_id != ctx.caller_id
    {
        return exact;
    }
    let ranked_slice = ranked.unwrap_or(entries);
    let visible: Vec<ThreadAddressEntry> = if opts.all_scope {
        ranked_slice.to_vec()
    } else {
        let local: Vec<ThreadAddressEntry> = ranked_slice
            .iter()
            .filter(|e| !e.is_remote())
            .cloned()
            .collect();
        let mut out = workspace_entries(&local, Some(&ctx.caller_workspace_root));
        out.extend(ranked_slice.iter().filter(|e| e.is_remote()).cloned());
        out
    };
    let filtered: Vec<ThreadAddressEntry> = visible
        .into_iter()
        .filter(|e| e.summary.thread_id != ctx.caller_id)
        .collect();
    match fuzzy_match(&filtered, text) {
        ThreadFuzzyResult::Ok { entry, .. } => LadderResult::Ok {
            entry,
            resolution: Resolution::Name,
        },
        ThreadFuzzyResult::Error(err) => LadderResult::Error(err),
    }
}

pub fn default_id_prefix() -> IdPrefixMode {
    IdPrefixMode::Enabled
}

pub async fn ladder_async(
    ctx: &ResolveContext,
    entries: &[ThreadAddressEntry],
    text: &str,
    opts: &ResolveOptions,
    ranked: Option<&[ThreadAddressEntry]>,
) -> LadderResult {
    let ctx = ctx.clone();
    let entries = entries.to_vec();
    let text = text.to_owned();
    let opts = opts.clone();
    let ranked = ranked.map(<[ThreadAddressEntry]>::to_vec);
    tokio::task::spawn_blocking(move || ladder(&ctx, &entries, &text, &opts, ranked.as_deref()))
        .await
        .expect("thread address worker failed")
}
