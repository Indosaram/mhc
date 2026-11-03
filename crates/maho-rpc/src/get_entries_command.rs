//! The one `get_entries` answer (senpi `get-entries-command.ts`): the range rules (`since`, `before`), the forward
//! (`limit`) and backward (`tail`, senpi#4267) pages and the byte budget (senpi#3534) are the same on every surface.
//!
//! senpi's page-walk hold of the full-history view (`holdEntryWalk`) has no counterpart: this port's session manager
//! keeps its entries resident, so there is no view to pin or release between pages.
use crate::get_entries_page::{
    EntryPageRequest, current_response_byte_budget, entry_page_bounds, page_entries,
    page_entries_from_end,
};
use crate::rpc_types::{
    RPC_ERROR_ENTRY_NOT_FOUND, RPC_ERROR_INVALID_ARGUMENT, RPC_ERROR_RESPONSE_TOO_LARGE,
};
use serde_json::{Value, json};

/// The arguments of one `get_entries`.
#[derive(Debug, Clone, Default)]
pub struct GetEntriesArgs {
    pub since: Option<String>,
    pub before: Option<String>,
    pub page: EntryPageRequest,
}

/// A refused read: the message and its stable `errorCode`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetEntriesRefusal {
    pub message: String,
    pub code: &'static str,
}

fn refuse(message: String, code: &'static str) -> GetEntriesRefusal {
    GetEntriesRefusal { message, code }
}

/// Answers `get_entries` over a session's entries in append order and its leaf id.
pub fn answer_get_entries(
    all: &[Value],
    leaf_id: Option<&str>,
    args: &GetEntriesArgs,
) -> Result<Value, GetEntriesRefusal> {
    let position = |id: &str| {
        all.iter()
            .position(|entry| entry["id"].as_str() == Some(id))
    };
    let mut end = all.len();
    if let Some(before) = &args.before {
        end = position(before).ok_or_else(|| {
            refuse(
                format!("Entry not found: {before}"),
                RPC_ERROR_ENTRY_NOT_FOUND,
            )
        })?;
    }
    let mut start = 0;
    if let Some(since) = &args.since {
        start = position(since).ok_or_else(|| {
            refuse(
                format!("Entry not found: {since}"),
                RPC_ERROR_ENTRY_NOT_FOUND,
            )
        })? + 1;
    }
    let entries = if start <= end {
        &all[start..end]
    } else {
        &all[..0]
    };
    let bounds = entry_page_bounds(&args.page)
        .map_err(|message| refuse(message, RPC_ERROR_INVALID_ARGUMENT))?;
    let project = maho_core::retry_fallback::usage_limit::describe_usage_limit_entry;
    let backward = bounds.tail.is_some();
    let (page, has_more, next_since, next_before, first_entry_bytes) = if backward {
        let page = page_entries_from_end(entries, project, bounds.tail, bounds.max_bytes);
        (
            page.entries,
            page.has_more,
            None,
            page.next_before,
            page.first_entry_bytes,
        )
    } else {
        let page = page_entries(entries, project, bounds.limit, bounds.max_bytes);
        (
            page.entries,
            page.has_more,
            page.next_since,
            None,
            page.first_entry_bytes,
        )
    };
    let budget = current_response_byte_budget();
    if first_entry_bytes > budget {
        let entry = if backward {
            entries.last()
        } else {
            entries.first()
        };
        let id = entry
            .and_then(|entry| entry["id"].as_str())
            .unwrap_or_default();
        return Err(refuse(
            format!(
                "Entry {id} alone is larger than one answer can carry ({budget} bytes), so it cannot be sent over RPC."
            ),
            RPC_ERROR_RESPONSE_TOO_LARGE,
        ));
    }
    if !bounds.paged && has_more {
        return Err(refuse(
            format!(
                "The session history is larger than one answer can carry ({budget} bytes). Read it in pages: pass limit or maxBytes, then since: nextSince until hasMore is false."
            ),
            RPC_ERROR_RESPONSE_TOO_LARGE,
        ));
    }
    let mut data = json!({"entries": page, "leafId": leaf_id});
    if bounds.paged {
        data["hasMore"] = has_more.into();
    }
    if let Some(next_since) = next_since {
        data["nextSince"] = next_since.into();
    }
    if let Some(next_before) = next_before {
        data["next_before"] = next_before.into();
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entries() -> Vec<Value> {
        (0..4)
            .map(|i| json!({"id":format!("e{i}"),"type":"custom"}))
            .collect()
    }
    #[test]
    fn before_and_tail_walk_backward() {
        let args = GetEntriesArgs {
            before: Some("e3".into()),
            page: EntryPageRequest {
                tail: Some(json!(2)),
                ..Default::default()
            },
            ..Default::default()
        };
        let data = answer_get_entries(&entries(), Some("e3"), &args).unwrap();
        assert_eq!(data["entries"].as_array().unwrap().len(), 2);
        assert_eq!(data["entries"][0]["id"], "e1");
        assert_eq!(data["next_before"], "e1");
        assert_eq!(data["hasMore"], true);
        assert_eq!(data["leafId"], "e3");
    }
    #[test]
    fn unknown_cursor_and_bad_bounds_are_coded() {
        let args = GetEntriesArgs {
            before: Some("nope".into()),
            ..Default::default()
        };
        assert_eq!(
            answer_get_entries(&entries(), None, &args)
                .unwrap_err()
                .code,
            RPC_ERROR_ENTRY_NOT_FOUND
        );
        let args = GetEntriesArgs {
            page: EntryPageRequest {
                limit: Some(json!(1)),
                tail: Some(json!(1)),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            answer_get_entries(&entries(), None, &args)
                .unwrap_err()
                .code,
            RPC_ERROR_INVALID_ARGUMENT
        );
    }
    #[test]
    fn an_unpaged_read_past_the_budget_is_refused() {
        let _budget = crate::get_entries_page::set_response_byte_budget_for_testing(40);
        let refused = answer_get_entries(&entries(), None, &GetEntriesArgs::default()).unwrap_err();
        assert_eq!(refused.code, RPC_ERROR_RESPONSE_TOO_LARGE);
        let since = GetEntriesArgs {
            since: Some("e0".into()),
            page: EntryPageRequest {
                limit: Some(json!(1)),
                ..Default::default()
            },
            ..Default::default()
        };
        let data = answer_get_entries(&entries(), None, &since).unwrap();
        assert_eq!(data["entries"][0]["id"], "e1");
        assert_eq!(data["nextSince"], "e1");
    }
}
