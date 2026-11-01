//! Supervisor owner memory over session-routed `ExtensionRequest`s (capabilities
//! `memory_receipts` and `memory_forget`).
//!
//! - `memory_receipts`: `{"action":"teach","sourceId","text"}` learns one owner-authored item;
//!   `{"action":"get","sourceId"|"receiptId"}` and `{"action":"list","limit"?}` read receipts.
//! - `memory_forget`: `{"sourceId","receiptId"|"text","reason"?}` removes the content from the
//!   memory repository and records replay suppression. Its response lists `possibleRestatements`:
//!   the memory files this call found still containing a line of the content inside a longer line
//!   (a superstring such as "Never use X" for "Use X"), which a forget deliberately keeps.
//! - `memory_correct`: `{"sourceId","receiptId"|"text","replacement","reason"?}` forgets the old
//!   content exactly like a forget and teaches the replacement as a new receipt that `supersedes`
//!   the old one. `sourceId` is always the owner message carrying the request.
//!
//! A request names no session: it is answered for the session whose runner received it, so one
//! session never acts on another session's binding. Teach, forget and correct are memory
//! mutations, refused with error code `memory_write_denied` in a memory-write-denied session
//! (`crate::write_policy`); reads stay open. None of them is a tool, so model or worker text can
//! never invoke one.
//!
//! Receipts are idempotent by the owner's source message id ([`owner_receipt_id`]). `accepted` is
//! written before the memory commit and `persisted` (naming that commit) after it: a crash in
//! between leaves the receipt `accepted`, and a retry finishes it without a second commit.
//! `applied` names a later session whose recalled prompt contained the item; it is not proof the
//! model obeyed it. Suppression lives only in mhc memory storage (`memory_core::receipts::owner`)
//! and is honored by facts extraction, reflection, dream and kibitzer delivery.
//!
//! Ordering. A teach's commit and a forget's or correct's suppress-remove-close all run inside the
//! identity's memory-write lock, and so do the facts apply and the reflection/dream integration,
//! each re-reading the suppression store there. A teach re-checks its receipt under that lock: a
//! forget or correct that closed it after `accepted` wins, and the item is neither committed nor
//! its suppression lifted. A teach lifts only a suppression whose line equals a taught line
//! exactly, never one closing its own receipt, and only after its commit landed, so a failed
//! teach never unsuppresses anything.
//!
//! Forget semantics (the receipt contract):
//! - It removes the taught file and every body line that restates the content EXACTLY (case,
//!   whitespace, markdown decoration and trailing punctuation aside) from the editable memory
//!   files at HEAD, and rewrites an exactly restating frontmatter `description` (rendered into
//!   every prompt) to a neutral one; "Never use X" survives a forget of "Use X". It never edits
//!   another receipt's `system/owner/` file, a `read_only: true` file, a file without
//!   frontmatter or a file that is not UTF-8 (inspected through a lossy decoding, as compile reads
//!   it), and cannot inspect an unreadable one: when one of those (may) still restate the content,
//!   the request fails with `memory_content_retained` naming the paths, after recording the
//!   suppression and removing what it could; the receipt stays open (never reported `forgotten`
//!   or `corrected` while the content is still recalled). Every edit is planned before the first
//!   write, and any failure restores the edited files, so a failed forget never leaves the
//!   repository dirty for the next writer.
//! - Every session whose prompt was pinned before the removal repins to the new HEAD on its next
//!   turn (`crate::projection_pin`), live or resumed.
//! - Replay suppression catches restatements only: facts records, lines a reflection or dream run
//!   adds, nudges and the transcript text handed to the facts child are withheld when they equal
//!   or contain a suppressed line. A paraphrase is guarded only by the do-not-record list given to
//!   the facts, reflection and dream children, which a model may disobey.
//! - It is not erasure. The memory repository's git history keeps the commits that added and
//!   removed the content (history views, sync, backup and restore can surface it); the receipt
//!   and suppression stores keep the text in plaintext in the identity runtime directory because
//!   matching needs it; a running session's own transcript and compaction summaries keep whatever
//!   was said in that conversation; and the memory tool or MCP memory server of a write-enabled
//!   session can write the content again (only extraction paths consult suppression).
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{EventKind, EventResult, ExtensionApi, ExtensionEvent, ExtensionFailure};
use memory_core::facts::FactsQueueEntry;
use memory_core::git::GitMemoryRepo;
use memory_core::git::repo_types::GitLogOptions;
use memory_core::journal::entries::TranscriptEntry;
use memory_core::receipts::owner::{
    NEUTRAL_DESCRIPTION, OwnerReceiptEvent, OwnerReceiptStore, OwnerReceiptView, RECEIPT_ACCEPTED, RECEIPT_APPLIED, RECEIPT_CORRECTED,
    RECEIPT_FORGOTTEN, RECEIPT_PERSISTED, SUPPRESSION_CORRECT, SUPPRESSION_FORGET, SuppressionSet, append_suppressions, canonical_text,
    lift_suppressions, owner_receipt_id, record_purge_if_changed, restates, restates_exactly, suppression_lines, text_sha256,
};
use memory_core::recall::RecallNudge;
use memory_core::tools::memfs::{parse_memory_file, render_memory_file};
use memory_core::tools::memory::{MemoryToolParams, run_memory_tool};
use serde_json::{Map, Value, json};

use crate::context::MemoryIdentityContext;
use crate::engine_session::{MemoryEngineSession, prepare_memory_engine_session};
use crate::index::MemoryComponent;

pub const MEMORY_RECEIPTS_REQUEST: &str = "memory_receipts";
pub const MEMORY_FORGET_REQUEST: &str = "memory_forget";
pub const MEMORY_CORRECT_REQUEST: &str = "memory_correct";
/// Taught items live at `system/owner/<receipt id>.md`, so every later prompt recalls them.
pub const OWNER_MEMORY_DIR: &str = "system/owner";

pub const INVALID_REQUEST_CODE: &str = "invalid_request";
pub const MEMORY_UNAVAILABLE_CODE: &str = "memory_unavailable";
pub const RECEIPT_NOT_FOUND_CODE: &str = "receipt_not_found";
pub const RECEIPT_CLOSED_CODE: &str = "receipt_closed";
pub const SOURCE_CONFLICT_CODE: &str = "source_conflict";
pub const MEMORY_WRITE_FAILED_CODE: &str = "memory_write_failed";
pub const MEMORY_STORAGE_FAILED_CODE: &str = "memory_storage_failed";
/// A forget or correct could not remove every restatement (see the module docs); suppression is
/// recorded and the receipt stays open.
pub const MEMORY_CONTENT_RETAINED_CODE: &str = "memory_content_retained";

const SOURCE_ID_MAX_CHARS: usize = 512;
const TEXT_MAX_CHARS: usize = 8000;
const REASON_MAX_CHARS: usize = 400;
const LIST_LIMIT_DEFAULT: u64 = 100;
const LIST_LIMIT_MAX: u64 = 1000;
const COMMIT_SEARCH_LIMIT: usize = 500;

/// A typed failure: the code travels as the RPC `errorCode`.
pub fn owner_memory_failure(code: &str, message: impl std::fmt::Display) -> ExtensionFailure {
    ExtensionFailure::classified(message.to_string(), None, Some(code.to_owned()))
}

fn invalid(message: impl std::fmt::Display) -> ExtensionFailure {
    owner_memory_failure(INVALID_REQUEST_CODE, message)
}

fn storage(error: String) -> ExtensionFailure {
    owner_memory_failure(MEMORY_STORAGE_FAILED_CODE, error)
}

fn write_failed(error: impl std::fmt::Display) -> ExtensionFailure {
    owner_memory_failure(MEMORY_WRITE_FAILED_CODE, error)
}

fn to_json<T: serde::Serialize + ?Sized>(value: &T) -> Result<Value, ExtensionFailure> {
    serde_json::to_value(value).map_err(|error| storage(error.to_string()))
}

/// The request object with only `allowed` keys (typed payloads deny unknown fields).
struct Fields<'a>(&'a Map<String, Value>);

fn fields<'a>(data: &'a Value, allowed: &[&str]) -> Result<Fields<'a>, ExtensionFailure> {
    let map = data.as_object().ok_or_else(|| invalid("request data must be a JSON object"))?;
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(format!("unknown field \"{key}\"")));
    }
    Ok(Fields(map))
}

impl Fields<'_> {
    fn text(&self, key: &str, max: usize) -> Result<Option<String>, ExtensionFailure> {
        match self.0.get(key) {
            None => Ok(None),
            Some(Value::String(value)) => {
                let value = canonical_text(value);
                if value.is_empty() {
                    return Err(invalid(format!("\"{key}\" must not be blank")));
                }
                if value.chars().count() > max {
                    return Err(invalid(format!("\"{key}\" is longer than {max} characters")));
                }
                if value.contains('\0') {
                    return Err(invalid(format!("\"{key}\" contains a NUL character")));
                }
                Ok(Some(value))
            }
            Some(_) => Err(invalid(format!("\"{key}\" must be a string"))),
        }
    }

    fn required(&self, key: &str, max: usize) -> Result<String, ExtensionFailure> {
        self.text(key, max)?.ok_or_else(|| invalid(format!("\"{key}\" is required")))
    }

    fn id(&self, key: &str) -> Result<Option<String>, ExtensionFailure> {
        let value = self.text(key, SOURCE_ID_MAX_CHARS)?;
        if value.as_deref().is_some_and(|value| value.chars().any(char::is_control)) {
            return Err(invalid(format!("\"{key}\" contains a control character")));
        }
        Ok(value)
    }

    fn required_id(&self, key: &str) -> Result<String, ExtensionFailure> {
        self.id(key)?.ok_or_else(|| invalid(format!("\"{key}\" is required")))
    }

    /// Exactly one of `receiptId` (an owner receipt) or `text` (content learned any other way).
    fn target(&self) -> Result<Target, ExtensionFailure> {
        match (self.id("receiptId")?, self.text("text", TEXT_MAX_CHARS)?) {
            (Some(receipt), None) => Ok(Target::Receipt(receipt)),
            (None, Some(text)) => Ok(Target::Text(text)),
            _ => Err(invalid("exactly one of \"receiptId\" or \"text\" is required")),
        }
    }
}

enum Target {
    Receipt(String),
    Text(String),
}

/// The session a request was routed to and its memory binding.
struct Bound {
    context: MemoryIdentityContext,
    session_id: String,
    profile_denied: bool,
}

impl Bound {
    fn refuse_writes(&self, what: &str) -> Result<(), ExtensionFailure> {
        if self.profile_denied || self.context.memory_write_denied {
            return Err(crate::write_policy::memory_write_denied_failure(what));
        }
        Ok(())
    }

    fn store(&self) -> OwnerReceiptStore {
        OwnerReceiptStore::new(&self.context.identity_paths.runtime)
    }

    fn engine(&self) -> Result<MemoryEngineSession, ExtensionFailure> {
        prepare_memory_engine_session(&self.context.identity, &self.context.identity_paths, Default::default())
            .map_err(|error| owner_memory_failure(MEMORY_UNAVAILABLE_CODE, error.message))
    }

    fn view(&self, receipt_id: &str) -> Result<OwnerReceiptView, ExtensionFailure> {
        self.store().view(receipt_id).map_err(storage)?.ok_or_else(|| owner_memory_failure(RECEIPT_NOT_FOUND_CODE, format!("no owner memory receipt {receipt_id}")))
    }

    fn runtime(&self) -> &Path {
        &self.context.identity_paths.runtime
    }
}

/// Runs `operation` inside the identity's memory-write lock, keeping its typed failure.
fn under_memory_write<T>(engine: &MemoryEngineSession, operation: impl FnOnce() -> Result<T, ExtensionFailure>) -> Result<T, ExtensionFailure> {
    engine.lock.run("memory-write", || Ok::<_, String>(operation())).map_err(|error| write_failed(error.message))?
}

#[derive(Clone, Copy)]
enum Op {
    Receipts,
    Forget,
    Correct,
}

/// Runs before each planned file write or delete of a forget's or correct's removal, with its
/// path; an error fails that write. Production passes [`no_write_fault`].
type BeforeWrite<'a> = &'a mut dyn FnMut(&str) -> Result<(), String>;

fn no_write_fault(_: &str) -> Result<(), String> {
    Ok(())
}

fn handle(op: Op, bound: &Bound, data: Value, before_write: BeforeWrite<'_>) -> Result<Value, ExtensionFailure> {
    let data = if data.is_null() { json!({}) } else { data };
    let mut response = match op {
        Op::Receipts => {
            let action = match data.get("action") {
                None => "list",
                Some(Value::String(action)) => action.as_str(),
                Some(_) => return Err(invalid("\"action\" must be a string")),
            };
            match action {
                "teach" => {
                    bound.refuse_writes("memory_receipts teach")?;
                    let request = fields(&data, &["action", "sourceId", "text"])?;
                    let source_id = request.required_id("sourceId")?;
                    let text = request.required("text", TEXT_MAX_CHARS)?;
                    let (receipt, changed) = teach(bound, &source_id, &text, None, true, &mut || {})?;
                    json!({"receipt":to_json(&receipt)?,"changed":changed})
                }
                "get" => {
                    let request = fields(&data, &["action", "sourceId", "receiptId"])?;
                    let receipt_id = match (request.id("sourceId")?, request.id("receiptId")?) {
                        (Some(source), None) => owner_receipt_id(&source),
                        (None, Some(receipt)) => receipt,
                        _ => return Err(invalid("exactly one of \"sourceId\" or \"receiptId\" is required")),
                    };
                    let receipt = bound.store().view(&receipt_id).map_err(storage)?;
                    json!({"receipt":to_json(&receipt)?})
                }
                "list" => {
                    let request = fields(&data, &["action", "limit"])?;
                    let limit = match request.0.get("limit") {
                        None => LIST_LIMIT_DEFAULT,
                        Some(value) => value.as_u64().filter(|limit| (1..=LIST_LIMIT_MAX).contains(limit)).ok_or_else(|| invalid(format!("\"limit\" must be an integer from 1 to {LIST_LIMIT_MAX}")))?,
                    };
                    let views = bound.store().views().map_err(storage)?;
                    let skip = views.len().saturating_sub(usize::try_from(limit).unwrap_or(usize::MAX));
                    json!({"receipts":to_json(&views[skip..])?})
                }
                other => return Err(invalid(format!("unknown action \"{other}\"; expected teach, get or list"))),
            }
        }
        Op::Forget => {
            bound.refuse_writes(MEMORY_FORGET_REQUEST)?;
            let request = fields(&data, &["sourceId", "receiptId", "text", "reason"])?;
            let source_id = request.required_id("sourceId")?;
            let target = request.target()?;
            let reason = request.text("reason", REASON_MAX_CHARS)?;
            let (receipt, changed, possible) = close(bound, &source_id, target, RECEIPT_FORGOTTEN, None, reason, before_write)?;
            json!({"receipt":to_json(&receipt)?,"changed":changed,"possibleRestatements":possible})
        }
        Op::Correct => {
            bound.refuse_writes(MEMORY_CORRECT_REQUEST)?;
            let request = fields(&data, &["sourceId", "receiptId", "text", "replacement", "reason"])?;
            let source_id = request.required_id("sourceId")?;
            let target = request.target()?;
            let replacement = request.required("replacement", TEXT_MAX_CHARS)?;
            let reason = request.text("reason", REASON_MAX_CHARS)?;
            correct(bound, &source_id, target, &replacement, reason, before_write)?
        }
    };
    response["identity"] = bound.context.identity.clone().into();
    Ok(response)
}

/// Answers one owner memory request (`memory_receipts`, `memory_forget` or `memory_correct`) for
/// the session `session_id` already resolved to `context`, exactly as the registered handler does
/// (synchronously). `profile_denied` is the session profile's memory-write denial.
pub fn answer_owner_memory_request(name: &str, context: MemoryIdentityContext, session_id: &str, profile_denied: bool, data: Value) -> Result<Value, ExtensionFailure> {
    let op = match name {
        MEMORY_RECEIPTS_REQUEST => Op::Receipts,
        MEMORY_FORGET_REQUEST => Op::Forget,
        MEMORY_CORRECT_REQUEST => Op::Correct,
        other => return Err(invalid(format!("unknown owner memory request \"{other}\""))),
    };
    handle(op, &Bound { context, session_id: session_id.to_owned(), profile_denied }, data, &mut no_write_fault)
}

/// Test seam: one `memory_forget` for a write-enabled session, answered like
/// [`answer_owner_memory_request`], whose `before_write` runs before each planned file write or
/// delete of the removal (in tree order) with its path; an error it returns fails that write, so
/// a test can inject a failure on the Nth write. Production forgets never fault.
#[doc(hidden)]
pub fn forget_interleaved(context: MemoryIdentityContext, session_id: &str, data: Value, before_write: &mut dyn FnMut(&str) -> Result<(), String>) -> Result<Value, ExtensionFailure> {
    handle(Op::Forget, &Bound { context, session_id: session_id.to_owned(), profile_denied: false }, data, before_write)
}

/// Test seam: one `memory_receipts` teach for a write-enabled session whose `between` runs after
/// `accepted` is recorded and before the teach's memory-write critical section, where a
/// concurrent forget or correct can land. Production teaches run with an empty `between`.
#[doc(hidden)]
pub fn teach_interleaved(context: MemoryIdentityContext, session_id: &str, source_id: &str, text: &str, between: &mut dyn FnMut()) -> Result<Value, ExtensionFailure> {
    let bound = Bound { context, session_id: session_id.to_owned(), profile_denied: false };
    bound.refuse_writes("memory_receipts teach")?;
    let (receipt, changed) = teach(&bound, source_id, &canonical_text(text), None, true, between)?;
    Ok(json!({"receipt":to_json(&receipt)?,"changed":changed,"identity":bound.context.identity.clone()}))
}

/// Accept, then persist one owner-taught item. Returns the receipt and whether this call wrote.
fn teach(bound: &Bound, source_id: &str, text: &str, supersedes: Option<&str>, lift: bool, between: &mut dyn FnMut()) -> Result<(OwnerReceiptView, bool), ExtensionFailure> {
    let store = bound.store();
    let receipt_id = owner_receipt_id(source_id);
    let hash = text_sha256(text);
    match store.view(&receipt_id).map_err(storage)? {
        Some(view) if view.accepted_at.is_none() || view.text_sha256.as_deref() != Some(hash.as_str()) => {
            return Err(owner_memory_failure(SOURCE_CONFLICT_CODE, format!("source {source_id} already names receipt {receipt_id} with different content")));
        }
        // Idempotent replay: never re-persist, and never revive a forgotten or corrected item.
        Some(view) if view.is_closed() || view.persisted.is_some() => return Ok((view, false)),
        Some(_) => {}
        None => {
            let mut accepted = OwnerReceiptEvent::new(&receipt_id, RECEIPT_ACCEPTED);
            accepted.source_id = Some(source_id.to_owned());
            accepted.session_id = Some(bound.session_id.clone());
            accepted.text = Some(text.to_owned());
            accepted.text_sha256 = Some(hash);
            accepted.supersedes = supersedes.map(str::to_owned);
            store.append_once(accepted).map_err(storage)?;
        }
    }
    between();
    let engine = bound.engine()?;
    let path = format!("{OWNER_MEMORY_DIR}/{receipt_id}.md");
    under_memory_write(&engine, || {
        // Re-checked under the lock every forget and correct hold: one that closed this receipt
        // after `accepted` wins, so the item is never committed nor its suppression lifted.
        let view = bound.view(&receipt_id)?;
        if view.is_closed() || view.persisted.is_some() {
            return Ok((view, false));
        }
        // A crash after the commit but before the `persisted` line left the item committed.
        let commit = match committed_commit(&engine.repo, &path).map_err(write_failed)? {
            Some(commit) => commit,
            None => {
                let params = MemoryToolParams {
                    command: Some("create".into()),
                    reason: format!("owner teach {receipt_id}"),
                    file_path: Some(path.clone()),
                    description: Some(format!("Owner-taught memory {receipt_id}")),
                    file_text: Some(format!("{text}\n")),
                    ..Default::default()
                };
                let written = run_memory_tool(&engine.repo, &engine.author, &params, None).map_err(|error| write_failed(error.message))?;
                written.commit.ok_or_else(|| write_failed("the teach write produced no commit"))?.sha
            }
        };
        // An explicit new teach of suppressed content is the owner changing their mind. Lifted only
        // once the item is committed (a crash retry re-runs it before `persisted`).
        if lift {
            lift_suppressions(bound.runtime(), text, source_id, &receipt_id).map_err(storage)?;
        }
        let mut persisted = OwnerReceiptEvent::new(&receipt_id, RECEIPT_PERSISTED);
        persisted.commit = Some(commit);
        persisted.path = Some(path.clone());
        store.append_once(persisted).map_err(storage)?;
        Ok((bound.view(&receipt_id)?, true))
    })
}

/// The receipt `close` would answer without writing: closed by this request (or any forget, for a
/// forget), or a typed refusal when another request closed it differently. `None`: still open.
fn already_closed(bound: &Bound, receipt_id: &str, source_id: &str, event: &str, text_mode: bool) -> Result<Option<OwnerReceiptView>, ExtensionFailure> {
    if text_mode {
        return Ok(bound.store().view(receipt_id).map_err(storage)?.filter(OwnerReceiptView::is_closed));
    }
    let view = bound.view(receipt_id)?;
    if let Some(closed) = &view.closed {
        if closed.by_source_id.as_deref() == Some(source_id) || event == RECEIPT_FORGOTTEN {
            return Ok(Some(view));
        }
        return Err(owner_memory_failure(RECEIPT_CLOSED_CODE, format!("receipt {receipt_id} is already {}", closed.event)));
    }
    Ok(None)
}

/// Forget (or correct) a target: suppress, remove from the repository, close the receipt, all in
/// one memory-write critical section. Each step is idempotent, so a retry after an interruption
/// completes the same close. Returns the receipt, whether this call wrote the closing event and
/// the paths this call found still containing the content inside a longer line.
fn close(bound: &Bound, source_id: &str, target: Target, event: &str, replaced_by: Option<&str>, reason: Option<String>, before_write: BeforeWrite<'_>) -> Result<(OwnerReceiptView, bool, Vec<String>), ExtensionFailure> {
    let store = bound.store();
    let (receipt_id, text_mode) = match &target {
        Target::Receipt(receipt_id) => (receipt_id.clone(), false),
        Target::Text(_) => (owner_receipt_id(&format!("{event}:{source_id}")), true),
    };
    if let Some(view) = already_closed(bound, &receipt_id, source_id, event, text_mode)? {
        return Ok((view, false, Vec::new()));
    }
    let (text, taught_path) = match target {
        // A teach writes its item at this path; it may be committed before `persisted` exists.
        Target::Receipt(_) => (
            bound.view(&receipt_id)?.text.ok_or_else(|| owner_memory_failure(RECEIPT_NOT_FOUND_CODE, format!("receipt {receipt_id} carries no taught content")))?,
            Some(format!("{OWNER_MEMORY_DIR}/{receipt_id}.md")),
        ),
        Target::Text(text) => (text, None),
    };
    let kind = if event == RECEIPT_CORRECTED { SUPPRESSION_CORRECT } else { SUPPRESSION_FORGET };
    if suppression_lines(&text).is_empty() {
        return Err(invalid("the content to forget has no line to suppress"));
    }
    let engine = bound.engine()?;
    under_memory_write(&engine, || {
        if let Some(view) = already_closed(bound, &receipt_id, source_id, event, text_mode)? {
            return Ok((view, false, Vec::new()));
        }
        // Suppress first: every extraction path re-reads the store under this same lock.
        append_suppressions(bound.runtime(), kind, &text, Some(&receipt_id), source_id).map_err(storage)?;
        let Removal { commit, committed, retained, possible } = remove_content(&engine, taught_path.as_deref(), &text, &format!("owner {event} {receipt_id}"), before_write).map_err(write_failed)?;
        // Every prompt pinned before this removal repins to the new HEAD on its next turn. A forget
        // that removed nothing and left nothing behind keeps every pin (and prompt cache), and so
        // does a retry that neither committed nor left a different retained set than the last one.
        if commit.is_some() || !retained.is_empty() {
            record_purge_if_changed(bound.runtime(), &receipt_id, source_id, committed, &retained).map_err(storage)?;
        }
        if !retained.is_empty() {
            return Err(owner_memory_failure(MEMORY_CONTENT_RETAINED_CODE, format!(
                "the content is (or, in an unreadable file, may be) still in memory files this request may not edit: {}; its suppression is recorded and receipt {receipt_id} stays open (forget an owner receipt by its receiptId, or fix read-only, non-UTF-8 or unreadable files by hand, then retry)",
                retained.join(", ")
            )));
        }
        let mut closed = OwnerReceiptEvent::new(&receipt_id, event);
        closed.source_id = Some(source_id.to_owned());
        closed.commit = commit;
        closed.replaced_by = replaced_by.map(str::to_owned);
        closed.reason = reason;
        if text_mode {
            closed.text = Some(text.clone());
        }
        let changed = store.append_once(closed).map_err(storage)?;
        Ok((bound.view(&receipt_id)?, changed, possible))
    })
}

fn correct(bound: &Bound, source_id: &str, target: Target, replacement: &str, reason: Option<String>, before_write: BeforeWrite<'_>) -> Result<Value, ExtensionFailure> {
    let (target_id, old) = match &target {
        Target::Receipt(receipt_id) => (receipt_id.clone(), bound.view(receipt_id)?.text),
        Target::Text(text) => (owner_receipt_id(&format!("{RECEIPT_CORRECTED}:{source_id}")), Some(text.clone())),
    };
    // The replacement must not restate a corrected line exactly: the old content stays suppressed.
    let replaced = suppression_lines(replacement);
    if old.is_some_and(|old| suppression_lines(&old).iter().any(|line| replaced.contains(line))) {
        return Err(invalid("\"replacement\" restates the corrected content"));
    }
    let replacement_id = owner_receipt_id(source_id);
    let replacement_hash = text_sha256(replacement);
    // The replacement's source must be free (or already this correction's replacement) before the
    // old content is removed; otherwise a failing teach would leave the owner with neither.
    if let Some(existing) = bound.store().view(&replacement_id).map_err(storage)?
        && (existing.supersedes.as_deref() != Some(target_id.as_str()) || existing.text_sha256.as_deref() != Some(replacement_hash.as_str()))
    {
        return Err(owner_memory_failure(SOURCE_CONFLICT_CODE, format!("source {source_id} already names receipt {replacement_id} with other content")));
    }
    let (corrected, closed, possible) = close(bound, source_id, target, RECEIPT_CORRECTED, Some(&replacement_id), reason, before_write)?;
    let (receipt, taught) = teach(bound, source_id, replacement, Some(&corrected.receipt_id), false, &mut || {})?;
    Ok(json!({"corrected":to_json(&corrected)?,"receipt":to_json(&receipt)?,"changed":closed||taught,"possibleRestatements":possible}))
}

/// The commit that last touched `path` when it is committed at HEAD.
fn committed_commit(repo: &GitMemoryRepo, path: &str) -> Result<Option<String>, String> {
    if !repo.dir.join(path).is_file() {
        return Ok(None);
    }
    let commits = repo.log(Some(&GitLogOptions { paths: Some(vec![path.to_owned()]), limit: Some(1), ..Default::default() })).map_err(|error| error.to_string())?;
    Ok(commits.into_iter().next().map(|commit| commit.sha))
}

/// `text` without its non-blank lines restating one of `lines` exactly; `None` when nothing matched.
fn drop_lines(text: &str, lines: &[String]) -> Option<String> {
    let mut dropped = false;
    let kept: Vec<&str> = text.split('\n').filter(|line| {
        let hit = !line.trim().is_empty() && lines.iter().any(|suppressed| restates_exactly(suppressed, line));
        dropped |= hit;
        !hit
    }).collect();
    dropped.then(|| kept.join("\n"))
}

/// Whether `line` restates one of `lines` exactly.
fn restating_exactly(lines: &[String], line: &str) -> bool {
    lines.iter().any(|suppressed| restates_exactly(suppressed, line))
}

/// Whether `line` restates one of `lines` or contains one (see [`restates`]).
fn restating(lines: &[String], line: &str) -> bool {
    lines.iter().any(|suppressed| restates(suppressed, line))
}

/// What [`remove_content`] removed and left.
struct Removal {
    /// The removal commit; with nothing left to remove, the commit of an interrupted earlier attempt.
    commit: Option<String>,
    /// Whether this call made `commit`.
    committed: bool,
    /// Files that (may) still restate the content exactly and that this writer may not edit.
    retained: Vec<String>,
    /// Files still containing a line of the content inside a longer line (kept on purpose).
    possible: Vec<String>,
}

/// Deletes the taught file and every body line restating `text` exactly from the editable tracked
/// memory files, and rewrites an exactly restating `description` to [`NEUTRAL_DESCRIPTION`], in
/// one commit named `subject`; the caller holds the memory-write lock. Another receipt's
/// `system/owner/` file, a `read_only: true` file (the pre-commit hook refuses to change it) and a
/// file without frontmatter (not a memory file this writer can re-render) are never edited, nor is
/// a file that is not UTF-8 (read through a lossy decoding, exact for every valid UTF-8 byte, and
/// never rewritten); those still restating the content are retained, and so is a file this writer
/// cannot read at all (it cannot tell whether the content is in it).
///
/// Every edit is planned (read, parse, render) before the first write; a failure while writing or
/// committing (`before_write` runs before each write and may fail it) restores every planned
/// path, so the repository is clean again whenever this returns an error.
fn remove_content(engine: &MemoryEngineSession, taught_path: Option<&str>, text: &str, subject: &str, before_write: BeforeWrite<'_>) -> Result<Removal, String> {
    let lines = suppression_lines(text);
    let owner_prefix = format!("{OWNER_MEMORY_DIR}/");
    engine.repo.clean_check().map_err(|error| error.to_string())?;
    // `None`: delete the file.
    let mut edits: Vec<(String, Option<String>)> = Vec::new();
    let mut retained: Vec<String> = Vec::new();
    let mut possible: Vec<String> = Vec::new();
    let still_contains = |description: Option<&str>, body: &str| description.is_some_and(|description| restating(&lines, description)) || body.split('\n').any(|line| restating(&lines, line));
    for path in engine.repo.ls_tree(Some("HEAD"), None).map_err(|error| error.to_string())?.into_iter().filter(|path| path.ends_with(".md")) {
        if taught_path == Some(path.as_str()) {
            edits.push((path, None));
            continue;
        }
        let Ok(bytes) = std::fs::read(engine.repo.dir.join(&path)) else {
            retained.push(path);
            continue;
        };
        let (content, utf8) = match String::from_utf8(bytes) {
            Ok(content) => (content, true),
            Err(error) => (String::from_utf8_lossy(error.as_bytes()).into_owned(), false),
        };
        let parsed = parse_memory_file(&content).ok();
        let editable = utf8 && parsed.as_ref().is_some_and(|parsed| parsed.frontmatter.read_only.as_deref() != Some("true") && !path.starts_with(&owner_prefix));
        let description = parsed.as_ref().map(|parsed| parsed.frontmatter.description.clone());
        let body = parsed.as_ref().map_or_else(|| content.clone(), |parsed| parsed.body.clone());
        let description_hit = description.as_deref().is_some_and(|description| restating_exactly(&lines, description));
        let dropped = drop_lines(&body, &lines);
        let (Some(mut frontmatter), true) = (parsed.map(|parsed| parsed.frontmatter), editable) else {
            if description_hit || dropped.is_some() {
                retained.push(path);
            } else if still_contains(description.as_deref(), body.as_str()) {
                possible.push(path);
            }
            continue;
        };
        if description_hit {
            if restating_exactly(&lines, NEUTRAL_DESCRIPTION) {
                retained.push(path);
                continue;
            }
            frontmatter.description = NEUTRAL_DESCRIPTION.to_owned();
        }
        let body_changed = dropped.is_some();
        let body = dropped.unwrap_or(body);
        if still_contains(Some(frontmatter.description.as_str()), body.as_str()) {
            possible.push(path.clone());
        }
        if !description_hit && !body_changed {
            continue;
        }
        match render_memory_file(&frontmatter, &body) {
            Ok(rendered) => edits.push((path, Some(rendered))),
            Err(_) => retained.push(path),
        }
    }
    if edits.is_empty() {
        let commits = engine.repo.log(Some(&GitLogOptions { limit: Some(COMMIT_SEARCH_LIMIT), ..Default::default() })).map_err(|error| error.to_string())?;
        let commit = commits.into_iter().find(|commit| commit.subject == subject).map(|commit| commit.sha);
        return Ok(Removal { commit, committed: false, retained, possible });
    }
    let paths: Vec<String> = edits.iter().map(|(path, _)| path.clone()).collect();
    let written = edits.iter().try_for_each(|(path, content)| {
        before_write(path.as_str())?;
        let file = engine.repo.dir.join(path);
        let result = match content {
            Some(content) => std::fs::write(&file, content),
            None => std::fs::remove_file(&file),
        };
        result.map_err(|error| format!("{path}: {error}"))
    });
    match written.and_then(|()| engine.repo.commit_write(paths.as_slice(), subject, &engine.author).map_err(|error| error.to_string())) {
        Ok(result) => Ok(Removal { commit: Some(result.sha), committed: true, retained, possible }),
        Err(detail) => {
            memory_core::tools::restore_paths(&engine.repo, &paths).map_err(|restore| format!("{detail}; restoring the edited files failed: {restore}"))?;
            Err(detail)
        }
    }
}

/// Records `applied` for every live persisted receipt whose content this session's prompt
/// recalled. Returns how many receipts it marked.
pub fn record_applied(context: &MemoryIdentityContext, session_id: &str, prompt: &str) -> Result<usize, String> {
    let store = OwnerReceiptStore::new(&context.identity_paths.runtime);
    if !store.path().exists() {
        return Ok(0);
    }
    let recalled: Vec<OwnerReceiptView> = store.views()?.into_iter().filter(|view| {
        !view.is_closed()
            && view.persisted.is_some()
            && !view.applied.iter().any(|mark| mark.session_id == session_id)
            && view.text.as_deref().is_some_and(|text| !text.is_empty() && prompt.contains(text))
    }).collect();
    if recalled.is_empty() {
        return Ok(0);
    }
    let head = GitMemoryRepo::open(&context.identity_paths.repo, &context.identity).and_then(|repo| repo.head()).map_err(|error| error.to_string())?;
    for view in &recalled {
        let mut applied = OwnerReceiptEvent::new(&view.receipt_id, RECEIPT_APPLIED);
        applied.session_id = Some(session_id.to_owned());
        applied.commit = head.clone();
        store.append_once(applied)?;
    }
    Ok(recalled.len())
}

/// Recall and kibitzer nudges without those restating forgotten or corrected content. An
/// unreadable suppression store withholds every nudge (fail closed) and warns.
pub fn drop_suppressed_nudges(context: &MemoryIdentityContext, source: &str, nudges: &[RecallNudge], warn: &dyn Fn(&str)) -> Vec<RecallNudge> {
    if nudges.is_empty() {
        return Vec::new();
    }
    match SuppressionSet::load(&context.identity_paths.runtime) {
        Ok(set) if set.is_empty() => nudges.to_vec(),
        Ok(set) => nudges.iter().filter(|nudge| !set.suppresses(&nudge.hint)).cloned().collect(),
        Err(error) => {
            warn(&format!("memory suppression unreadable; {source} nudges withheld: {error}"));
            Vec::new()
        }
    }
}

/// Facts queue entries whose transcript text no longer carries the lines restating forgotten or
/// corrected content: the input-side replay guard, applied before the facts child sees a batch.
/// Entry ids and ranges are kept, so consumption still marks the whole batch.
pub fn scrub_facts_entries(set: &SuppressionSet, mut entries: Vec<FactsQueueEntry>) -> Vec<FactsQueueEntry> {
    if set.is_empty() {
        return entries;
    }
    for queued in &mut entries {
        for entry in &mut queued.entries {
            if let TranscriptEntry::Text(text) = entry
                && let Some(clean) = set.scrub_transcript_text(&text.text)
            {
                text.text = clean;
            }
        }
    }
    entries
}

/// Registers the three requests and the `applied` observer for one session's runner. Call after
/// the memory prompt handler is registered, so the observer sees the recalled prompt.
pub fn register_owner_memory(api: &mut ExtensionApi, component: Arc<MemoryComponent>, warn: Arc<dyn Fn(&str) + Send + Sync>) -> Result<(), ExtensionFailure> {
    let profile_denied = crate::write_policy::profile_denies_memory_write(&api.profile);
    let session: Arc<Mutex<Option<String>>> = Arc::default();
    let resolve = Arc::new(move |id: &str| -> Option<MemoryIdentityContext> {
        component.sessions.lock().unwrap_or_else(PoisonError::into_inner).get(id).filter(|state| state.enabled).and_then(|state| state.context.clone())
    });
    let started = session.clone();
    api.on(EventKind::SessionStart, Arc::new(move |_, context| {
        *started.lock().unwrap_or_else(PoisonError::into_inner) = Some(context.session_manager.session_id().to_owned());
        Box::pin(async { Ok(EventResult::None) })
    }));
    let observe = resolve.clone();
    api.on(EventKind::BeforeAgentStart, Arc::new(move |event, context| {
        let session_id = context.session_manager.session_id().to_owned();
        let prompt = match event {
            ExtensionEvent::BeforeAgentStart(start) => Some(start.system_prompt.clone()),
            _ => None,
        };
        if let (Some(identity), Some(prompt)) = (observe(&session_id), prompt)
            && let Err(error) = record_applied(&identity, &session_id, &prompt)
        {
            warn(&format!("memory receipts: applied record failed for {session_id}: {error}"));
        }
        Box::pin(async { Ok(EventResult::None) })
    }));
    for (name, op) in [(MEMORY_RECEIPTS_REQUEST, Op::Receipts), (MEMORY_FORGET_REQUEST, Op::Forget), (MEMORY_CORRECT_REQUEST, Op::Correct)] {
        let session = session.clone();
        let resolve = resolve.clone();
        api.rpc_handle(name, Arc::new(move |data| {
            let session_id = session.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let result = match session_id.and_then(|id| resolve(&id).map(|context| (id, context))) {
                Some((session_id, context)) => handle(op, &Bound { context, session_id, profile_denied }, data, &mut no_write_fault),
                None => Err(owner_memory_failure(MEMORY_UNAVAILABLE_CODE, "no memory identity is bound to this session")),
            };
            Box::pin(async move { result })
        }))?;
    }
    Ok(())
}
