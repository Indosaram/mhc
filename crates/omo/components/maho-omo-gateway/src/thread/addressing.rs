//! Addressing and scope resolution (`thread/addressing.ts`).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::process::Command;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use super::address_core::{
    KeyedRow, NamedRow, is_id_prefix_shape, is_session_id_shape, is_thread_id_shape,
    normalize_name, row_key, row_name,
};
use super::contracts_results::{ThreadStatus, ThreadSummary};
use super::errors::{ThreadErrorCode, ThreadToolFailure, thread_tool_failure};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadAddressEntry {
    #[serde(flatten)]
    pub summary: ThreadSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ids: Option<Vec<String>>,
    /// Remote session transport tag (`"relay"` in upstream). `O01` local-only keeps this `None`
    /// unless a remote listing row is injected in tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_row_key: Option<String>,
}

impl ThreadAddressEntry {
    pub fn from_summary(summary: ThreadSummary) -> Self {
        Self {
            summary,
            preview: None,
            device: None,
            aliases: None,
            session_ids: None,
            remote_row_key: None,
        }
    }

    pub fn is_remote(&self) -> bool {
        self.remote_row_key.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadCandidate {
    pub thread: String,
    pub name: String,
    pub cwd: String,
    pub preview: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
    pub state: ThreadStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unnamed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadTargetResolution {
    Id,
    Alias,
    Name,
    IdPrefix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadFuzzyBasis {
    Name,
    Preview,
    Cwd,
}

#[derive(Debug, Clone, Default)]
pub enum IdPrefixMode {
    #[default]
    Enabled,
    Disabled,
    Exclude(String),
}

#[derive(Debug, Clone, Default)]
pub struct ThreadResolveOptions {
    pub all_scope: bool,
    pub caller_workspace_root: Option<String>,
    pub id_prefix: IdPrefixMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadAddressError {
    pub code: ThreadErrorCode,
    pub message: String,
    pub next_action: String,
    pub details: Option<serde_json::Value>,
    pub candidates: Option<Vec<ThreadCandidate>>,
}

impl ThreadAddressError {
    pub fn into_failure(self) -> ThreadToolFailure {
        ThreadToolFailure {
            code: self.code,
            message: self.message,
            next_action: self.next_action,
            details: self.details,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ThreadResolveResult {
    Ok {
        entry: ThreadAddressEntry,
        resolution: ThreadTargetResolution,
    },
    Error(ThreadAddressError),
}

#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ThreadFuzzyResult {
    Ok {
        entry: ThreadAddressEntry,
        score: f64,
        basis: ThreadFuzzyBasis,
    },
    Error(ThreadAddressError),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ThreadNameConflictResult {
    Ok,
    Error(ThreadAddressError),
}

const AMBIGUOUS_CANDIDATE_LIMIT: usize = 10;
const FUZZY_MIN_QUERY_LENGTH: usize = 3;
const FUZZY_MAX_CANDIDATES: usize = 500;
const FUZZY_PREVIEW_WEIGHT: f64 = 0.85;
const FUZZY_CWD_WEIGHT: f64 = 0.7;
const FUZZY_ACCEPT_SCORE: f64 = 0.72;
const FUZZY_ACCEPT_MARGIN: f64 = 0.08;

static UNSAFE_FOR_DISPLAY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]").expect("valid static UNSAFE_FOR_DISPLAY_RE")
});

/// Escapes `Cc`, `Cf`, `Zl`, and `Zp` characters as `\uXXXX` / `\u{XXXXX}` (`escape-for-display.ts`).
pub fn escape_for_display(text: &str) -> String {
    UNSAFE_FOR_DISPLAY_RE
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let ch = caps
                .get(0)
                .and_then(|m| m.as_str().chars().next())
                .unwrap_or('\0');
            let code = ch as u32;
            if code > 0xffff {
                format!("\\u{{{code:x}}}")
            } else {
                format!("\\u{code:04x}")
            }
        })
        .into_owned()
}

pub fn normalize_thread_name(value: &str) -> String {
    normalize_name(value)
}

fn has_unfoldable_controls(value: &str) -> bool {
    value.chars().any(|ch| {
        let code = ch as u32;
        matches!(code, 0x0000..=0x0008 | 0x000e..=0x001f | 0x007f)
    })
}

fn address_error(
    code: &str,
    message: impl Into<String>,
    next_action: impl Into<String>,
    candidates: Option<Vec<ThreadCandidate>>,
) -> ThreadAddressError {
    let details = candidates
        .as_ref()
        .map(|c| serde_json::json!({ "candidates": c }));
    let failure = thread_tool_failure(code, message, next_action, details);
    ThreadAddressError {
        code: failure.code,
        message: failure.message,
        next_action: failure.next_action,
        details: failure.details,
        candidates,
    }
}

#[allow(clippy::result_large_err)]
fn guard_input(value: &str, what: &str) -> Result<String, ThreadAddressError> {
    if has_unfoldable_controls(value) {
        return Err(address_error(
            "invalid_arguments",
            format!("The {what} contains control characters."),
            "Remove the control characters and retry, or pass an address from thread_list (tool.thread_list() in an eval cell).",
            None,
        ));
    }
    let normalized = normalize_thread_name(value);
    if normalized.is_empty() {
        return Err(address_error(
            "invalid_arguments",
            format!("The {what} is empty."),
            format!(
                "Pass a thread value or a unique name from thread_list (tool.thread_list() in an eval cell) as the {what}."
            ),
            None,
        ));
    }
    Ok(normalized)
}

fn candidate_thread(entry: &ThreadAddressEntry) -> String {
    if entry.summary.thread_id.is_empty()
        && let Some(key) = &entry.remote_row_key
    {
        return row_key(&KeyedRow {
            device: entry.device.as_deref().unwrap_or(""),
            durable_id: None,
            session_id: key,
        });
    }
    entry.summary.thread_id.clone()
}

pub fn to_candidate(entry: &ThreadAddressEntry) -> ThreadCandidate {
    let is_remote = entry.is_remote();
    let shown = |text: &str| -> String {
        if is_remote {
            escape_for_display(text)
        } else {
            text.to_owned()
        }
    };
    let named = row_name(&NamedRow {
        name: Some(entry.summary.name.as_str()),
        first_message: entry.preview.as_deref(),
        folder: entry.summary.cwd.as_str(),
    });
    ThreadCandidate {
        thread: candidate_thread(entry),
        name: shown(&named.name),
        cwd: shown(&entry.summary.cwd),
        preview: entry.preview.clone().unwrap_or_default(),
        updated_at: entry.summary.updated_at.clone(),
        state: entry.summary.status,
        device: entry.device.clone(),
        unnamed: named.unnamed.then_some(true),
        address: None,
        device_name: None,
    }
}

pub fn to_candidates(matches: &[ThreadAddressEntry]) -> Vec<ThreadCandidate> {
    let spans = matches.iter().any(|entry| entry.device.is_some());
    let mut tagged: Vec<ThreadAddressEntry> = matches
        .iter()
        .cloned()
        .map(|mut entry| {
            if spans && entry.device.is_none() && !entry.is_remote() {
                entry.device = Some("here".to_owned());
            }
            entry
        })
        .collect();
    tagged.sort_by(|a, b| {
        if a.is_remote() != b.is_remote() {
            return if !a.is_remote() {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        match (&a.summary.updated_at, &b.summary.updated_at) {
            (None, None) => a.summary.thread_id.cmp(&b.summary.thread_id),
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(ua), Some(ub)) => {
                if ua == ub {
                    a.summary.thread_id.cmp(&b.summary.thread_id)
                } else {
                    ub.cmp(ua)
                }
            }
        }
    });
    tagged
        .into_iter()
        .take(AMBIGUOUS_CANDIDATE_LIMIT)
        .map(|entry| to_candidate(&entry))
        .collect()
}

fn canonical_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned())
}

fn git_worktree_root(dir: &str) -> Option<String> {
    if dir.trim().is_empty() {
        return None;
    }
    let output = Command::new("git")
        .args(["-C", dir, "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let trimmed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn root_of(cache: &mut HashMap<String, Option<String>>, dir: &str) -> Option<String> {
    if let Some(cached) = cache.get(dir) {
        return cached.clone();
    }
    let root = git_worktree_root(dir);
    cache.insert(dir.to_owned(), root.clone());
    root
}

fn same_workspace(
    entry_cwd: &str,
    caller_root: &str,
    cache: &mut HashMap<String, Option<String>>,
) -> bool {
    let entry_root = root_of(cache, entry_cwd);
    let caller_top = root_of(cache, caller_root);
    if let (Some(er), Some(ct)) = (entry_root, caller_top) {
        return canonical_path(&er) == canonical_path(&ct);
    }
    canonical_path(entry_cwd) == canonical_path(caller_root)
}

/// Every path the caller's workspace can be spelled as: the root, its git top level, and their canonical forms.
pub fn workspace_directories(caller_workspace_root: &str) -> Vec<String> {
    let root = caller_workspace_root.trim();
    if root.is_empty() {
        return Vec::new();
    }
    let mut set = BTreeSet::new();
    let top = git_worktree_root(root);
    for p in std::iter::once(root).chain(top.as_deref()) {
        set.insert(p.to_owned());
        set.insert(canonical_path(p));
    }
    set.into_iter().collect()
}

/// The entries that belong to the caller's workspace.
pub fn workspace_entries(
    entries: &[ThreadAddressEntry],
    caller_workspace_root: Option<&str>,
) -> Vec<ThreadAddressEntry> {
    let caller_root = caller_workspace_root.map(str::trim).unwrap_or("");
    if caller_root.is_empty() {
        return Vec::new();
    }
    let mut cache = HashMap::new();
    entries
        .iter()
        .filter(|entry| same_workspace(&entry.summary.cwd, caller_root, &mut cache))
        .cloned()
        .collect()
}

pub async fn workspace_entries_async(
    entries: &[ThreadAddressEntry],
    caller_workspace_root: Option<&str>,
) -> Vec<ThreadAddressEntry> {
    let entries = entries.to_vec();
    let caller_workspace_root = caller_workspace_root.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        workspace_entries(&entries, caller_workspace_root.as_deref())
    })
    .await
    .expect("workspace lookup worker failed")
}

fn scoped_id_hit(
    entry: &ThreadAddressEntry,
    opts: &ThreadResolveOptions,
    caller_root: &str,
    cache: &mut HashMap<String, Option<String>>,
) -> ThreadResolveResult {
    if opts.all_scope || entry.is_remote() {
        return ThreadResolveResult::Ok {
            entry: entry.clone(),
            resolution: ThreadTargetResolution::Id,
        };
    }
    if caller_root.is_empty() {
        return ThreadResolveResult::Error(address_error(
            "caller_context_missing",
            "Scoping an id hit needs the caller's workspace root, which this call did not provide.",
            "Set all_scope to address every workspace, or call thread_list (tool.thread_list() in an eval cell) from a session that knows its workspace.",
            None,
        ));
    }
    if same_workspace(&entry.summary.cwd, caller_root, cache) {
        return ThreadResolveResult::Ok {
            entry: entry.clone(),
            resolution: ThreadTargetResolution::Id,
        };
    }
    ThreadResolveResult::Error(address_error(
        "scope_denied",
        format!(
            "Thread {} exists outside this workspace.",
            entry.summary.thread_id
        ),
        "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to confirm the id, then retry with all_scope set, or target a thread in this workspace.",
        None,
    ))
}

fn not_found(target: &str, scope: &str, visible: &[ThreadAddressEntry]) -> ThreadAddressError {
    let base = format!("No visible thread matches \"{target}\" in {scope}.");
    if is_thread_id_shape(target) {
        return address_error(
            "not_found",
            format!("{base} No reachable endpoint lists a thread with that thread_id."),
            "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to check the id; a thread on a device that is offline is found once it is online.",
            None,
        );
    }
    if !is_session_id_shape(target) {
        return address_error(
            "not_found",
            base,
            "Call thread_list (tool.thread_list() in an eval cell) and pass a row's thread value or a unique name it returned.",
            None,
        );
    }
    let ids: Vec<&str> = visible
        .iter()
        .map(|e| e.summary.thread_id.as_str())
        .filter(|id| !id.is_empty())
        .take(5)
        .collect();
    let listed = if ids.is_empty() {
        String::new()
    } else {
        format!(" Visible thread values: {}.", ids.join(", "))
    };
    address_error(
        "not_found",
        format!(
            "{base} \"{target}\" is a session id, not a thread value. Pass the thread value thread_list (tool.thread_list() in an eval cell) shows, or omit thread / pass self for this session.{listed}"
        ),
        "Call thread_list (tool.thread_list() in an eval cell) and pass a row's thread value.",
        None,
    )
}

/// Resolve a thread address (`id`, `session_id`, `alias`, `name`, or `id_prefix`).
pub fn resolve_target(
    entries: &[ThreadAddressEntry],
    target: &str,
    opts: &ThreadResolveOptions,
) -> ThreadResolveResult {
    let normalized = match guard_input(target, "thread address") {
        Ok(n) => n,
        Err(err) => return ThreadResolveResult::Error(err),
    };
    let caller_root = opts
        .caller_workspace_root
        .as_deref()
        .map(str::trim)
        .unwrap_or("");
    let has_caller_root = !caller_root.is_empty();
    let mut cache = HashMap::new();

    if let Some(id_hit) = entries.iter().find(|e| e.summary.thread_id == target) {
        return scoped_id_hit(id_hit, opts, caller_root, &mut cache);
    }

    let session_hits: Vec<&ThreadAddressEntry> = entries
        .iter()
        .filter(|e| {
            e.session_ids
                .as_ref()
                .is_some_and(|ids| ids.iter().any(|id| id == target))
        })
        .collect();
    if session_hits.len() == 1 {
        let hit = scoped_id_hit(session_hits[0], opts, caller_root, &mut cache);
        if let ThreadResolveResult::Error(ref err) = hit
            && err.code.as_str() == "scope_denied"
        {
            return ThreadResolveResult::Error(address_error(
                "scope_denied",
                format!(
                    "Thread {} (session id \"{target}\") exists outside this workspace.",
                    session_hits[0].summary.thread_id
                ),
                format!(
                    "Call thread_list with all_scope (tool.thread_list({{ all_scope: true }}) in an eval cell) and pass thread {}, or target a thread in this workspace.",
                    session_hits[0].summary.thread_id
                ),
                None,
            ));
        }
        return hit;
    }
    if session_hits.len() >= 2 {
        let owned: Vec<ThreadAddressEntry> = session_hits.iter().map(|e| (*e).clone()).collect();
        let ids: Vec<String> = owned
            .iter()
            .map(candidate_thread)
            .filter(|id| !id.is_empty())
            .collect();
        return ThreadResolveResult::Error(address_error(
            "ambiguous_target",
            format!(
                "{} visible threads share the session id \"{target}\". Pass one thread: {}.",
                owned.len(),
                ids.join(", ")
            ),
            "Call thread_list (tool.thread_list() in an eval cell) and pass the exact thread value of the intended thread.",
            Some(to_candidates(&owned)),
        ));
    }

    let wanted_alias = normalize_thread_name(target);
    if !wanted_alias.is_empty()
        && let Some(alias_hit) = entries.iter().find(|e| {
            e.aliases.as_ref().is_some_and(|aliases| {
                aliases
                    .iter()
                    .any(|a| normalize_thread_name(a) == wanted_alias)
            })
        })
    {
        return ThreadResolveResult::Ok {
            entry: alias_hit.clone(),
            resolution: ThreadTargetResolution::Alias,
        };
    }

    let visible: Vec<ThreadAddressEntry> = if opts.all_scope {
        entries.to_vec()
    } else {
        if !has_caller_root {
            return ThreadResolveResult::Error(address_error(
                "caller_context_missing",
                "Scoped address resolution needs the caller's workspace root, which this call did not provide.",
                "Set all_scope to address every workspace, or call thread_list (tool.thread_list() in an eval cell) from a session that knows its workspace.",
                None,
            ));
        }
        entries
            .iter()
            .filter(|e| e.is_remote() || same_workspace(&e.summary.cwd, caller_root, &mut cache))
            .cloned()
            .collect()
    };

    let matches: Vec<ThreadAddressEntry> = visible
        .iter()
        .filter(|e| normalize_thread_name(&e.summary.name) == normalized)
        .cloned()
        .collect();
    if matches.len() == 1 {
        return ThreadResolveResult::Ok {
            entry: matches[0].clone(),
            resolution: ThreadTargetResolution::Name,
        };
    }
    if matches.len() >= 2 {
        return ThreadResolveResult::Error(address_error(
            "ambiguous_target",
            format!(
                "{} visible threads share the name \"{target}\".",
                matches.len()
            ),
            "Call thread_list (tool.thread_list() in an eval cell) and pass the exact thread value of the intended thread.",
            Some(to_candidates(&matches)),
        ));
    }

    if !matches!(opts.id_prefix, IdPrefixMode::Disabled) && is_id_prefix_shape(target) {
        let prefix = target.to_lowercase();
        let excluded = match &opts.id_prefix {
            IdPrefixMode::Exclude(id) => Some(id.as_str()),
            _ => None,
        };
        let mut by_thread: HashMap<String, ThreadAddressEntry> = HashMap::new();
        for entry in entries {
            if Some(entry.summary.thread_id.as_str()) != excluded
                && entry.summary.thread_id.to_lowercase().starts_with(&prefix)
                && !by_thread.contains_key(&entry.summary.thread_id)
            {
                by_thread.insert(entry.summary.thread_id.clone(), entry.clone());
            }
        }
        let prefixed: Vec<ThreadAddressEntry> = by_thread.into_values().collect();
        if prefixed.len() == 1 {
            return match scoped_id_hit(&prefixed[0], opts, caller_root, &mut cache) {
                ThreadResolveResult::Ok { entry, .. } => ThreadResolveResult::Ok {
                    entry,
                    resolution: ThreadTargetResolution::IdPrefix,
                },
                err => err,
            };
        }
        if prefixed.len() >= 2 {
            return ThreadResolveResult::Error(address_error(
                "ambiguous_target",
                format!(
                    "{} threads have an id starting with \"{target}\".",
                    prefixed.len()
                ),
                "Pass a longer prefix or the exact thread value from the candidates.",
                Some(to_candidates(&prefixed)),
            ));
        }
    }

    if !opts.all_scope && has_caller_root && matches.is_empty() {
        let out_of_scope: Vec<ThreadAddressEntry> = entries
            .iter()
            .filter(|e| {
                !e.is_remote()
                    && !same_workspace(&e.summary.cwd, caller_root, &mut cache)
                    && normalize_thread_name(&e.summary.name) == normalized
            })
            .cloned()
            .collect();
        if !out_of_scope.is_empty() {
            let msg = if out_of_scope.len() == 1 {
                format!("A thread named \"{target}\" exists on this device outside this workspace.")
            } else {
                format!(
                    "{} threads named \"{target}\" exist on this device outside this workspace.",
                    out_of_scope.len()
                )
            };
            return ThreadResolveResult::Error(address_error(
                "scope_denied",
                msg,
                "Call thread_list with all_scope (tool.thread_list({ all_scope: true }) in an eval cell) to see it, then retry with all_scope set, or target a thread in this workspace.",
                Some(to_candidates(&out_of_scope)),
            ));
        }
    }

    ThreadResolveResult::Error(not_found(
        target,
        if opts.all_scope {
            "the daemon-wide scope"
        } else {
            "this workspace"
        },
        &visible,
    ))
}

fn trigrams(value: &str) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 3 {
        return Vec::new();
    }
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

fn dice_coefficient(left: &[String], right: &[String]) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for gram in left {
        *counts.entry(gram.as_str()).or_insert(0) += 1;
    }
    let mut shared = 0usize;
    for gram in right {
        if let Some(remaining) = counts.get_mut(gram.as_str())
            && *remaining > 0
        {
            shared += 1;
            *remaining -= 1;
        }
    }
    (2 * shared) as f64 / (left.len() + right.len()) as f64
}

fn score_entry(entry: &ThreadAddressEntry, query_grams: &[String]) -> (f64, ThreadFuzzyBasis) {
    let name_score = dice_coefficient(
        query_grams,
        &trigrams(&normalize_thread_name(&entry.summary.name)),
    );
    let preview_norm = entry
        .preview
        .as_deref()
        .map(normalize_thread_name)
        .unwrap_or_default();
    let preview_score = if preview_norm.is_empty() {
        0.0
    } else {
        FUZZY_PREVIEW_WEIGHT * dice_coefficient(query_grams, &trigrams(&preview_norm))
    };
    let cwd_base = Path::new(&entry.summary.cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&entry.summary.cwd);
    let cwd_score = FUZZY_CWD_WEIGHT
        * dice_coefficient(query_grams, &trigrams(&normalize_thread_name(cwd_base)));

    let mut score = name_score;
    let mut basis = ThreadFuzzyBasis::Name;
    if preview_score > score {
        score = preview_score;
        basis = ThreadFuzzyBasis::Preview;
    }
    if cwd_score > score {
        score = cwd_score;
        basis = ThreadFuzzyBasis::Cwd;
    }
    (score, basis)
}

pub fn fuzzy_match(entries: &[ThreadAddressEntry], query: &str) -> ThreadFuzzyResult {
    let normalized = match guard_input(query, "fuzzy query") {
        Ok(n) => n,
        Err(err) => return ThreadFuzzyResult::Error(err),
    };
    if normalized.chars().count() < FUZZY_MIN_QUERY_LENGTH {
        return ThreadFuzzyResult::Error(address_error(
            "invalid_arguments",
            format!(
                "A fuzzy query needs at least {FUZZY_MIN_QUERY_LENGTH} characters; \"{query}\" is too short."
            ),
            "Pass a longer query, or call thread_list (tool.thread_list() in an eval cell) and pass a row's thread value or a unique name it returned.",
            None,
        ));
    }
    if entries.len() > FUZZY_MAX_CANDIDATES {
        return ThreadFuzzyResult::Error(address_error(
            "ambiguous_target",
            format!(
                "{} threads are in scope; the fuzzy scan is capped at {FUZZY_MAX_CANDIDATES}.",
                entries.len()
            ),
            "Call thread_list (tool.thread_list() in an eval cell), pick the intended thread, and pass its thread value instead of a fuzzy query.",
            None,
        ));
    }
    if entries.is_empty() {
        return ThreadFuzzyResult::Error(address_error(
            "not_found",
            "There is no thread to match.",
            "Call thread_list (tool.thread_list() in an eval cell) to see the addressable threads.",
            None,
        ));
    }

    let query_grams = trigrams(&normalized);
    let mut scored: Vec<(&ThreadAddressEntry, f64, ThreadFuzzyBasis)> = entries
        .iter()
        .map(|entry| {
            let (score, basis) = score_entry(entry, &query_grams);
            (entry, score, basis)
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.summary.thread_id.cmp(&b.0.summary.thread_id))
    });

    let leader = scored[0];
    let runner_up = scored.get(1).map(|s| s.1).unwrap_or(0.0);
    if leader.1 < FUZZY_ACCEPT_SCORE {
        return ThreadFuzzyResult::Error(address_error(
            "not_found",
            format!(
                "No thread scored close enough to \"{query}\" (best {:.3}).",
                leader.1
            ),
            "Call thread_list (tool.thread_list() in an eval cell) and pass a row's thread value, or retry with a longer, more distinctive query.",
            None,
        ));
    }
    if leader.1 - runner_up < FUZZY_ACCEPT_MARGIN {
        let cands: Vec<ThreadAddressEntry> = scored
            .iter()
            .take(AMBIGUOUS_CANDIDATE_LIMIT)
            .map(|(e, _, _)| (*e).clone())
            .collect();
        return ThreadFuzzyResult::Error(address_error(
            "ambiguous_target",
            format!(
                "Several threads match \"{query}\" about equally (best {:.3}, runner-up {:.3}).",
                leader.1, runner_up
            ),
            "Call thread_list (tool.thread_list() in an eval cell) and pass the exact thread value of the intended thread.",
            Some(to_candidates(&cands)),
        ));
    }
    ThreadFuzzyResult::Ok {
        entry: leader.0.clone(),
        score: leader.1,
        basis: leader.2,
    }
}

pub fn fuzzy_contenders(entries: &[ThreadAddressEntry], query: &str) -> Vec<ThreadAddressEntry> {
    let Ok(normalized) = guard_input(query, "fuzzy query") else {
        return Vec::new();
    };
    if normalized.chars().count() < FUZZY_MIN_QUERY_LENGTH {
        return Vec::new();
    }
    let query_grams = trigrams(&normalized);
    entries
        .iter()
        .filter(|e| score_entry(e, &query_grams).0 >= FUZZY_ACCEPT_SCORE - FUZZY_ACCEPT_MARGIN)
        .cloned()
        .collect()
}

pub fn check_name_conflict(entries: &[ThreadSummary], name: &str) -> ThreadNameConflictResult {
    let normalized = match guard_input(name, "thread name") {
        Ok(n) => n,
        Err(err) => return ThreadNameConflictResult::Error(err),
    };
    if let Some(conflict) = entries
        .iter()
        .find(|e| normalize_thread_name(&e.name) == normalized)
    {
        let cand = to_candidate(&ThreadAddressEntry::from_summary(conflict.clone()));
        return ThreadNameConflictResult::Error(address_error(
            "name_conflict",
            format!(
                "A thread named \"{}\" ({}) already exists.",
                conflict.name, conflict.thread_id
            ),
            "Call thread_list (tool.thread_list() in an eval cell) to reuse that thread by id, or pass a different name for the new thread.",
            Some(vec![cand]),
        ));
    }
    ThreadNameConflictResult::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: &str, name: &str, cwd: &str) -> ThreadSummary {
        ThreadSummary {
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
        }
    }

    #[test]
    fn resolves_by_id_name_and_prefix_with_workspace_scope() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let a = dir_a.path().to_string_lossy().into_owned();
        let b = dir_b.path().to_string_lossy().into_owned();
        let entries = vec![
            ThreadAddressEntry::from_summary(summary(
                "01965d50-0000-7000-8000-000000000001",
                "Alpha",
                &a,
            )),
            ThreadAddressEntry::from_summary(summary(
                "01965d50-0000-7000-8000-000000000002",
                "Beta",
                &b,
            )),
        ];
        match resolve_target(
            &entries,
            "Alpha",
            &ThreadResolveOptions {
                all_scope: false,
                caller_workspace_root: Some(a.clone()),
                id_prefix: IdPrefixMode::Enabled,
            },
        ) {
            ThreadResolveResult::Ok { resolution, .. } => {
                assert_eq!(resolution, ThreadTargetResolution::Name)
            }
            other => panic!("expected Ok, got {other:?}"),
        }
        match resolve_target(
            &entries,
            "Beta",
            &ThreadResolveOptions {
                all_scope: false,
                caller_workspace_root: Some(a),
                id_prefix: IdPrefixMode::Enabled,
            },
        ) {
            ThreadResolveResult::Error(err) => assert_eq!(err.code.as_str(), "scope_denied"),
            other => panic!("expected scope_denied, got {other:?}"),
        }
    }

    #[test]
    fn resolves_by_nfkc_case_and_collapsed_whitespace() {
        assert_eq!(normalize_thread_name("  Build \t Runner "), "build runner");
        let entries = vec![ThreadAddressEntry::from_summary(summary(
            "01965d50-0000-7000-8000-000000000001",
            "Build   Runner",
            "/repo",
        ))];
        match resolve_target(
            &entries,
            " build runner ",
            &ThreadResolveOptions {
                all_scope: true,
                caller_workspace_root: None,
                id_prefix: IdPrefixMode::Enabled,
            },
        ) {
            ThreadResolveResult::Ok { resolution, .. } => {
                assert_eq!(resolution, ThreadTargetResolution::Name)
            }
            other => panic!("expected Name match, got {other:?}"),
        }
    }

    #[test]
    fn refuses_empty_and_control_char_targets() {
        let entries = Vec::new();
        for bad in ["", "   ", "bad\x07bell"] {
            match resolve_target(&entries, bad, &ThreadResolveOptions::default()) {
                ThreadResolveResult::Error(e) => assert_eq!(e.code.as_str(), "invalid_arguments"),
                other => panic!("expected invalid_arguments for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn resolves_by_session_id_and_denies_cross_workspace_id() {
        let mut s = summary("01965d50-0000-7000-8000-000000000010", "Other", "/other");
        s.cwd = "/other".into();
        let mut entry = ThreadAddressEntry::from_summary(s);
        entry.session_ids = Some(vec!["rpc-10".into()]);
        match resolve_target(
            &[entry],
            "rpc-10",
            &ThreadResolveOptions {
                all_scope: false,
                caller_workspace_root: Some("/mine".into()),
                ..Default::default()
            },
        ) {
            ThreadResolveResult::Error(e) => assert_eq!(e.code.as_str(), "scope_denied"),
            other => panic!("expected scope_denied, got {other:?}"),
        }
    }

    #[test]
    fn resolves_by_alias_before_workspace_scoping() {
        let mut entry = ThreadAddressEntry::from_summary(summary(
            "01965d50-0000-7000-8000-000000000011",
            "ActualName",
            "/other",
        ));
        entry.aliases = Some(vec!["PinnedAlias".into()]);
        match resolve_target(&[entry], "pinnedalias", &ThreadResolveOptions::default()) {
            ThreadResolveResult::Ok { resolution, .. } => {
                assert_eq!(resolution, ThreadTargetResolution::Alias)
            }
            other => panic!("expected Alias match, got {other:?}"),
        }
    }

    #[test]
    fn ambiguous_name_returns_candidates() {
        let entries = vec![
            ThreadAddressEntry::from_summary(summary("id-1", "Dup", "/repo")),
            ThreadAddressEntry::from_summary(summary("id-2", "dup", "/repo")),
        ];
        match resolve_target(
            &entries,
            "Dup",
            &ThreadResolveOptions {
                all_scope: true,
                ..Default::default()
            },
        ) {
            ThreadResolveResult::Error(e) => {
                assert_eq!(e.code.as_str(), "ambiguous_target");
                assert_eq!(e.candidates.unwrap().len(), 2);
            }
            other => panic!("expected ambiguous_target, got {other:?}"),
        }
    }

    #[test]
    fn id_prefix_resolves_unique_and_respects_disabled_mode() {
        let entries = vec![ThreadAddressEntry::from_summary(summary(
            "01965d50-aaaa-7000-8000-000000000001",
            "PrefixTarget",
            "/repo",
        ))];
        match resolve_target(
            &entries,
            "01965d50-aaaa",
            &ThreadResolveOptions {
                all_scope: true,
                id_prefix: IdPrefixMode::Enabled,
                ..Default::default()
            },
        ) {
            ThreadResolveResult::Ok { resolution, .. } => {
                assert_eq!(resolution, ThreadTargetResolution::IdPrefix)
            }
            other => panic!("expected IdPrefix, got {other:?}"),
        }
        match resolve_target(
            &entries,
            "01965d50-aaaa",
            &ThreadResolveOptions {
                all_scope: true,
                id_prefix: IdPrefixMode::Disabled,
                ..Default::default()
            },
        ) {
            ThreadResolveResult::Error(e) => assert_eq!(e.code.as_str(), "not_found"),
            other => panic!("expected not_found when disabled, got {other:?}"),
        }
    }

    #[test]
    fn missing_caller_root_refuses_name_lookup_and_all_scope_resolves_self_id() {
        let entries = vec![ThreadAddressEntry::from_summary(summary(
            "dur-self", "Self", "/repo",
        ))];
        match resolve_target(&entries, "Self", &ThreadResolveOptions::default()) {
            ThreadResolveResult::Error(e) => assert_eq!(e.code.as_str(), "caller_context_missing"),
            other => panic!("expected caller_context_missing, got {other:?}"),
        }
        match resolve_target(
            &entries,
            "dur-self",
            &ThreadResolveOptions {
                all_scope: true,
                ..Default::default()
            },
        ) {
            ThreadResolveResult::Ok { resolution, .. } => {
                assert_eq!(resolution, ThreadTargetResolution::Id)
            }
            other => panic!("expected Id, got {other:?}"),
        }
    }

    #[test]
    fn fuzzy_match_accepts_clear_winner_and_rejects_short_queries() {
        let entries = vec![
            ThreadAddressEntry::from_summary(summary("id-1", "checkout-billing", "/repo")),
            ThreadAddressEntry::from_summary(summary("id-2", "docs-readme", "/repo")),
        ];
        match fuzzy_match(&entries, "ab") {
            ThreadFuzzyResult::Error(e) => assert_eq!(e.code.as_str(), "invalid_arguments"),
            other => panic!("expected invalid_arguments for short query, got {other:?}"),
        }
        match fuzzy_match(&entries, "checkout-bill") {
            ThreadFuzzyResult::Ok { entry, .. } => assert_eq!(entry.summary.thread_id, "id-1"),
            other => panic!("expected fuzzy hit, got {other:?}"),
        }
    }

    #[test]
    fn name_conflict_detection_blocks_duplicate_names() {
        let entries = vec![summary("id-1", "Worker", "/repo")];
        assert!(matches!(
            check_name_conflict(&entries, "other"),
            ThreadNameConflictResult::Ok
        ));
        assert!(matches!(
            check_name_conflict(&entries, " worker "),
            ThreadNameConflictResult::Error(ref e) if e.code.as_str() == "name_conflict"
        ));
    }

    #[test]
    fn escape_for_display_neutralizes_control_characters() {
        let escaped = escape_for_display("hi\x1b[31m\u{202e}");
        assert!(!escaped.contains('\x1b'));
        assert!(escaped.contains("\\u001b"));
        assert!(escaped.contains("\\u202e"));
    }

    #[test]
    fn workspace_entries_filters_matching_root_only() {
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        let p1 = d1.path().to_string_lossy().into_owned();
        let p2 = d2.path().to_string_lossy().into_owned();
        let entries = vec![
            ThreadAddressEntry::from_summary(summary("1", "A", &p1)),
            ThreadAddressEntry::from_summary(summary("2", "B", &p2)),
        ];
        let scoped = workspace_entries(&entries, Some(&p1));
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].summary.thread_id, "1");
    }

    #[test]
    fn session_id_not_found_gives_actionable_thread_hint() {
        let entries = vec![ThreadAddressEntry::from_summary(summary(
            "01965d50-0000-7000-8000-000000000001",
            "Alpha",
            "/repo",
        ))];
        match resolve_target(
            &entries,
            "rpc-999",
            &ThreadResolveOptions {
                all_scope: true,
                ..Default::default()
            },
        ) {
            ThreadResolveResult::Error(e) => {
                assert_eq!(e.code.as_str(), "not_found");
                assert!(e.message.contains("session id"));
            }
            other => panic!("expected not_found with session id hint, got {other:?}"),
        }
    }
}
