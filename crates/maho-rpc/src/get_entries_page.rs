//! Bounded `get_entries` answers (senpi `get-entries-page.ts`, senpi#3534). One response line rides the
//! connection's event queue, which fails the whole connection closed past 64 MiB, so a history read is cut into
//! pages a client walks with `since` (forward) or `before` (backward, `tail`, senpi#4267), and an unpaged read that
//! would not fit is refused with `response_too_large` instead of taking the connection down.
use serde_json::Value;

/// The largest answer one unpaged read may produce: well under the 64 MiB connection queue bound.
pub const RPC_RESPONSE_BYTE_BUDGET: usize = 48 * 1024 * 1024;
pub const DEFAULT_ENTRY_PAGE_BYTES: usize = 8 * 1024 * 1024;
/// How long a page walk may pause before the full-history view it holds is let go (senpi `holdEntryWalk`).
///
/// NATIVE-DIVERGENCE(senpi `holdEntryWalk`/`endEntryWalk`): upstream's paged session manager materializes a
/// full-history view for a walk and must drop it after this idle window. The native session manager keeps its
/// entries resident and every page reads them in place, so a walk holds nothing between pages and there is no view
/// to release; the constant is kept for wire/parity reference only.
pub const ENTRY_WALK_HOLD_MS: u64 = 30_000;

thread_local! {
    // Per thread so a test lowering it never changes another test's (or the host's) answers.
    static RESPONSE_BYTE_BUDGET: std::cell::Cell<usize> = const { std::cell::Cell::new(RPC_RESPONSE_BYTE_BUDGET) };
}

/// The byte budget one answer may use; tests lower it to reach the refusal without a 48 MiB history.
pub fn current_response_byte_budget() -> usize {
    RESPONSE_BYTE_BUDGET.with(std::cell::Cell::get)
}

/// Lowers this thread's budget for a test; the returned guard restores the previous one.
pub fn set_response_byte_budget_for_testing(bytes: usize) -> ResponseBudgetGuard {
    ResponseBudgetGuard(RESPONSE_BYTE_BUDGET.with(|budget| budget.replace(bytes)))
}
pub struct ResponseBudgetGuard(usize);
impl Drop for ResponseBudgetGuard {
    fn drop(&mut self) {
        RESPONSE_BYTE_BUDGET.with(|budget| budget.set(self.0));
    }
}

#[derive(Debug, Clone, Default)]
pub struct EntryPageRequest {
    pub limit: Option<Value>,
    pub tail: Option<Value>,
    pub max_bytes: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPageBounds {
    pub paged: bool,
    pub limit: Option<usize>,
    pub tail: Option<usize>,
    pub max_bytes: usize,
}

fn positive_integer(value: Option<&Value>) -> Result<Option<usize>, ()> {
    match value {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|value| *value > 0 && *value <= 9_007_199_254_740_991)
            .and_then(|value| usize::try_from(value).ok())
            .map(Some)
            .ok_or(()),
    }
}

pub fn entry_page_bounds(request: &EntryPageRequest) -> Result<EntryPageBounds, String> {
    let limit = positive_integer(request.limit.as_ref())
        .map_err(|()| "limit must be a positive integer".to_owned())?;
    let tail = positive_integer(request.tail.as_ref())
        .map_err(|()| "tail must be a positive integer".to_owned())?;
    if limit.is_some() && tail.is_some() {
        return Err("limit and tail cannot be combined: limit pages forward, tail backward".into());
    }
    let max_bytes = positive_integer(request.max_bytes.as_ref())
        .map_err(|()| "maxBytes must be a positive integer".to_owned())?;
    let budget = current_response_byte_budget();
    if limit.is_none() && tail.is_none() && max_bytes.is_none() {
        return Ok(EntryPageBounds {
            paged: false,
            limit: None,
            tail: None,
            max_bytes: budget,
        });
    }
    Ok(EntryPageBounds {
        paged: true,
        limit,
        tail,
        max_bytes: max_bytes.unwrap_or(DEFAULT_ENTRY_PAGE_BYTES).min(budget),
    })
}

fn serialized_size(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len()) + 1
}

#[derive(Debug, Clone, PartialEq)]
pub struct EntryPage {
    pub entries: Vec<Value>,
    pub has_more: bool,
    pub next_since: Option<String>,
    /// Serialized size of the page's first entry (0 for an empty page): one entry past the budget cannot be sent.
    pub first_entry_bytes: usize,
}

/// Takes entries in order until the count or the serialized byte size would pass the bounds. The first entry is
/// always taken, so a page always moves the cursor forward. Only the page is projected.
pub fn page_entries(
    entries: &[Value],
    project: impl Fn(Value) -> Value,
    limit: Option<usize>,
    max_bytes: usize,
) -> EntryPage {
    let mut page = Vec::new();
    let mut bytes = 0;
    let mut first_entry_bytes = 0;
    for entry in entries {
        if limit.is_some_and(|limit| page.len() >= limit) {
            break;
        }
        let projected = project(entry.clone());
        let size = serialized_size(&projected);
        if !page.is_empty() && bytes + size > max_bytes {
            break;
        }
        if page.is_empty() {
            first_entry_bytes = size;
        }
        page.push(projected);
        bytes += size;
    }
    let taken = page.len();
    let has_more = taken < entries.len();
    let next_since = (has_more && taken > 0)
        .then(|| entries[taken - 1]["id"].as_str().map(str::to_owned))
        .flatten();
    EntryPage {
        entries: page,
        has_more,
        next_since,
        first_entry_bytes,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TailEntryPage {
    /// In append order: the oldest of the page first.
    pub entries: Vec<Value>,
    pub has_more: bool,
    /// The oldest entry id the page returned, while older entries remain: pass it as the next `before`.
    pub next_before: Option<String>,
    /// Serialized size of the page's newest entry (0 for an empty page).
    pub first_entry_bytes: usize,
}

/// The newest entries first, until the count or the byte size would pass the bounds, answered in append order.
/// The newest entry is always taken, so a backward walk always moves.
pub fn page_entries_from_end(
    entries: &[Value],
    project: impl Fn(Value) -> Value,
    tail: Option<usize>,
    max_bytes: usize,
) -> TailEntryPage {
    let mut page = Vec::new();
    let mut bytes = 0;
    let mut first_entry_bytes = 0;
    let mut index = entries.len();
    while index > 0 {
        if tail.is_some_and(|tail| page.len() >= tail) {
            break;
        }
        let projected = project(entries[index - 1].clone());
        let size = serialized_size(&projected);
        if !page.is_empty() && bytes + size > max_bytes {
            break;
        }
        if page.is_empty() {
            first_entry_bytes = size;
        }
        page.push(projected);
        bytes += size;
        index -= 1;
    }
    page.reverse();
    let has_more = index > 0;
    let next_before = has_more
        .then(|| {
            entries
                .get(index)
                .and_then(|oldest| oldest["id"].as_str().map(str::to_owned))
        })
        .flatten();
    TailEntryPage {
        entries: page,
        has_more,
        next_before,
        first_entry_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn entries(count: usize) -> Vec<Value> {
        (0..count).map(|i| json!({"id":format!("e{i}")})).collect()
    }
    #[test]
    fn the_unpaged_budget_stays_under_the_connection_queue_bound() {
        assert_eq!(RPC_RESPONSE_BYTE_BUDGET, 48 * 1024 * 1024);
        const { assert!(RPC_RESPONSE_BYTE_BUDGET < crate::socket_event_fanout::DEFAULT_QUEUE_BYTES) };
        assert_eq!(current_response_byte_budget(), RPC_RESPONSE_BYTE_BUDGET);
        {
            let _lowered = set_response_byte_budget_for_testing(10);
            assert_eq!(current_response_byte_budget(), 10);
            let bounds = entry_page_bounds(&EntryPageRequest {
                limit: Some(json!(1)),
                ..Default::default()
            })
            .unwrap();
            assert_eq!(bounds.max_bytes, 10);
        }
        assert_eq!(current_response_byte_budget(), RPC_RESPONSE_BYTE_BUDGET);
    }
    #[test]
    fn bounds_reject_bad_values_and_combined_directions() {
        let request = |limit: Option<Value>, tail: Option<Value>| EntryPageRequest {
            limit,
            tail,
            max_bytes: None,
        };
        assert_eq!(
            entry_page_bounds(&request(None, Some(json!(0)))).unwrap_err(),
            "tail must be a positive integer"
        );
        assert!(
            entry_page_bounds(&request(Some(json!(1)), Some(json!(1))))
                .unwrap_err()
                .starts_with("limit and tail cannot be combined")
        );
        assert!(!entry_page_bounds(&request(None, None)).unwrap().paged);
        let tail = entry_page_bounds(&request(None, Some(json!(2)))).unwrap();
        assert_eq!(
            (tail.paged, tail.tail, tail.max_bytes),
            (true, Some(2), DEFAULT_ENTRY_PAGE_BYTES)
        );
    }
    #[test]
    fn a_tail_page_is_the_newest_in_append_order_with_a_before_cursor() {
        let page = page_entries_from_end(&entries(5), |e| e, Some(2), usize::MAX);
        assert_eq!(page.entries, vec![json!({"id":"e3"}), json!({"id":"e4"})]);
        assert!(page.has_more);
        assert_eq!(page.next_before.as_deref(), Some("e3"));
        let all = page_entries_from_end(&entries(2), |e| e, Some(5), usize::MAX);
        assert!(!all.has_more);
        assert_eq!(all.next_before, None);
    }
    #[test]
    fn a_forward_page_always_takes_one_entry_and_names_next_since() {
        let page = page_entries(&entries(3), |e| e, None, 1);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.next_since.as_deref(), Some("e0"));
        assert_eq!(page.first_entry_bytes, serialized_size(&json!({"id":"e0"})));
    }
}
