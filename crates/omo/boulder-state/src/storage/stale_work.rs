//! Port of `storage/stale-work.ts`: repair works abandoned by a dead session.
//!
//! A ulw-execute session that dies abnormally never completes its work, so `completeBoulder`
//! is never reached and the work stays `active` forever (#8413). Activity decides instead:
//! [`reconcile_stale_works`] demotes an `active` work to `paused` when its last activity — the
//! newest of its sessions' transcript mtimes, `updated_at` and `started_at` — is at least the
//! threshold old, and stamps `stale_since`.

use std::collections::BTreeMap;
use std::path::Path;

use crate::js::{Js, JsObj};
use crate::records::BoulderState;
use crate::shared::{parse_iso_to_ms, project_work_to_mirror, strip_session_platform};
use crate::storage::read_state::{read_boulder_state, work_docs};
use crate::storage::write_state::write_boulder_state;
use crate::time::{format_iso_millis, now_millis};
use crate::types::{
    ReconcileStaleWorksOptions, StaleWorkDemotion, StaleWorkReconcileResult,
};

/// A ulw-execute session that dies abnormally never completes its work, so activity decides.
pub const DEFAULT_STALE_WORK_THRESHOLD_MS: f64 = 21_600_000.0;
pub const STALE_WORK_THRESHOLD_ENV_KEY: &str = "OMO_BOULDER_STALE_WORK_THRESHOLD_MS";

const SESSION_TRANSCRIPT_SUFFIX: &str = ".jsonl";

/// The `NO_CHANGES` constant: nothing was stale, so nothing was written.
fn no_changes() -> StaleWorkReconcileResult {
    StaleWorkReconcileResult {
        demoted: Vec::new(),
        written: false,
    }
}

/// `resolveStaleWorkThresholdMs(env)`: the six-hour default unless the env knob holds a
/// positive, parsable number.
pub fn resolve_stale_work_threshold_ms(env: &BTreeMap<String, String>) -> f64 {
    let raw = env
        .get(STALE_WORK_THRESHOLD_ENV_KEY)
        .map(|value| value.trim())
        .unwrap_or_default();
    if raw.is_empty() {
        return DEFAULT_STALE_WORK_THRESHOLD_MS;
    }

    // Number accepts unsigned radix prefixes as well as decimal fractions/exponents.
    let radix = [("0x", "0X", 16), ("0b", "0B", 2), ("0o", "0O", 8)]
        .into_iter()
        .find_map(|(lower, upper, radix)| raw.strip_prefix(lower)
            .or_else(|| raw.strip_prefix(upper)).map(|digits| (digits, radix)));
    let parsed = match radix {
        Some((digits, radix)) if !digits.is_empty() => digits.chars().try_fold(0.0, |value, digit| {
            digit.to_digit(radix).map(|digit| value * f64::from(radix) + f64::from(digit))
        }),
        Some(_) => None,
        None => raw.parse::<f64>().ok(),
    };
    parsed.filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_STALE_WORK_THRESHOLD_MS)
}

/// Pure staleness rule: a work with no activity evidence at all is stale, and recorded activity
/// older than the threshold is stale. A stamp in the future (clock skew) counts as activity.
pub fn is_work_stale(last_activity_ms: Option<i64>, now_ms: i64, threshold_ms: f64) -> bool {
    is_work_stale_precise(last_activity_ms.map(|ms| ms as f64), now_ms, threshold_ms)
}

pub fn is_work_stale_precise(last_activity_ms: Option<f64>, now_ms: i64, threshold_ms: f64) -> bool {
    match last_activity_ms {
        None => true,
        Some(last) => now_ms as f64 - last >= threshold_ms,
    }
}

/// Demote every `active` work whose last activity is older than the threshold to `paused` and
/// stamp `stale_since`. Nothing stale means no write at all; `completed`/`abandoned` works and
/// works with no recorded status are never touched, and a failure of any kind is swallowed so a
/// read path can call this unconditionally.
pub fn reconcile_stale_works(
    directory: &Path,
    options: &ReconcileStaleWorksOptions,
) -> StaleWorkReconcileResult {
    // Every failure is swallowed, so the read path may call this unconditionally.
    reconcile_stale_works_inner(directory, options).unwrap_or_else(no_changes)
}

fn reconcile_stale_works_inner(
    directory: &Path,
    options: &ReconcileStaleWorksOptions,
) -> Option<StaleWorkReconcileResult> {
    let state = read_boulder_state(directory).ok()??;

    let now_ms = options.now.unwrap_or_else(now_millis);
    let threshold_ms = options.threshold_ms.unwrap_or_else(|| match &options.env {
        Some(env) => resolve_stale_work_threshold_ms(env),
        None => {
            // Only the one knob is read; snapshotting the whole process environment on every
            // read-path call is wasted work.
            let env: BTreeMap<String, String> = std::env::var_os(STALE_WORK_THRESHOLD_ENV_KEY)
                .and_then(|value| value.into_string().ok())
                .map(|value| (STALE_WORK_THRESHOLD_ENV_KEY.to_string(), value))
                .into_iter()
                .collect();
            resolve_stale_work_threshold_ms(&env)
        }
    });
    let works = work_docs(&state.doc);
    let stale_since = format_iso_millis(now_ms);
    let mut demoted: Vec<StaleWorkDemotion> = Vec::new();
    let mut demoted_works: BTreeMap<String, JsObj> = BTreeMap::new();

    for work in &works {
        if work.get_str("status") != Some("active") {
            continue;
        }

        let last_activity_ms = resolve_last_activity_ms(
            work,
            directory,
            now_ms,
            threshold_ms,
            options.sessions_directory.as_deref(),
        );
        if !is_work_stale_precise(last_activity_ms, now_ms, threshold_ms) {
            continue;
        }

        let work_id = work.field("work_id").to_js_string();
        let mut demoted_work = work.clone();
        demoted_work.set("status", Js::string("paused"));
        demoted_work.set("stale_since", Js::string(&stale_since));
        demoted_works.insert(work_id.clone(), demoted_work);
        demoted.push(StaleWorkDemotion {
            work_id,
            stale_since: stale_since.clone(),
            last_activity_at: last_activity_ms.map(|ms| format_iso_millis(ms.trunc() as i64)),
        });
    }

    if demoted.is_empty() {
        return Some(no_changes());
    }

    let mut next_works = JsObj::default();
    for work in &works {
        let work_id = work.field("work_id").to_js_string();
        match demoted_works.get(&work_id) {
            Some(demoted_work) => next_works.set(&work_id, Js::Object(demoted_work.clone())),
            None => next_works.set(&work_id, Js::Object(work.clone())),
        }
    }

    // A legacy mirror-only state carries its single work implicitly; persisting a demotion
    // materializes it exactly like selectActiveWork/addBoulderWork already do.
    let active_work_id = state
        .doc
        .get_str("active_work_id")
        .map(str::to_string)
        .or_else(|| {
            if works.len() == 1 {
                Some(works[0].field("work_id").to_js_string())
            } else {
                None
            }
        });

    let mut next_doc = state.doc.clone();
    next_doc.set("schema_version", Js::int(2));
    next_doc.set("works", Js::Object(next_works.clone()));
    if let Some(active_work_id) = &active_work_id {
        next_doc.set("active_work_id", Js::string(active_work_id));
    }

    if let Some(active_work_id) = &active_work_id
        && let Some(mirror_work) = next_works.get_obj(active_work_id)
    {
        project_work_to_mirror(&mut next_doc, mirror_work);
    }

    let next_state = BoulderState::from_doc(next_doc);
    match write_boulder_state(directory, &next_state) {
        Ok(()) => Some(StaleWorkReconcileResult {
            demoted,
            written: true,
        }),
        Err(_) => Some(no_changes()),
    }
}

fn resolve_last_activity_ms(
    work: &JsObj,
    directory: &Path,
    now_ms: i64,
    threshold_ms: f64,
    sessions_directory: Option<&Path>,
) -> Option<f64> {
    let recorded_ms = newest_ms([
        parse_iso_to_ms(work.get("updated_at")).map(|ms| ms as f64),
        parse_iso_to_ms(work.get("started_at")).map(|ms| ms as f64),
    ]);
    // Recorded activity inside the window already proves the work is not stale, so the transcript
    // scan only runs for works that would otherwise be demoted.
    if let Some(recorded_ms) = recorded_ms
        && now_ms as f64 - recorded_ms < threshold_ms
    {
        return Some(recorded_ms);
    }

    let Some(sessions_directory) = sessions_directory else {
        return recorded_ms;
    };

    let session_ids = crate::records::string_items(work, "session_ids");
    let worktree_path = work.get_str("worktree_path").map(str::to_string);
    newest_ms([
        recorded_ms,
        find_newest_session_transcript_ms_precise(
            sessions_directory,
            &session_ids,
            &[Some(directory.to_string_lossy().into_owned()), worktree_path],
        ),
    ])
}

/// Newest mtime of the transcripts belonging to `session_ids` under an agent sessions directory,
/// which stores them as `<encoded session cwd>/<timestamp>_<sessionId>.jsonl` (and, on older
/// layouts, flat at the root). Only session directories that look like one of `session_cwds` are
/// read: an agent home accumulates thousands of them and reading them all costs seconds.
pub fn find_newest_session_transcript_ms(
    sessions_directory: &Path,
    session_ids: &[String],
    session_cwds: &[Option<String>],
) -> Option<i64> {
    find_newest_session_transcript_ms_precise(sessions_directory, session_ids, session_cwds)
        .map(|ms| ms.trunc() as i64)
}

pub fn find_newest_session_transcript_ms_precise(
    sessions_directory: &Path,
    session_ids: &[String],
    session_cwds: &[Option<String>],
) -> Option<f64> {
    let session_ids: Vec<String> = session_ids
        .iter()
        .map(|session_id| strip_session_platform(session_id))
        .filter(|session_id| !session_id.is_empty())
        .collect();
    let needles: Vec<String> = session_cwds
        .iter()
        .filter_map(|cwd| cwd.as_deref())
        .filter(|cwd| !cwd.trim().is_empty())
        .map(normalize_path_for_match)
        .collect();
    if session_ids.is_empty() {
        return None;
    }

    let Ok(entries) = std::fs::read_dir(sessions_directory) else {
        return None;
    };

    let mut newest: Option<f64> = None;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_file() {
            if is_transcript_of(&name, &session_ids) {
                newest = newest_ms([newest, mtime_ms(&entry.path())]);
            }
            continue;
        }

        if !file_type.is_dir() || !matches_session_cwd(&name, &needles) {
            continue;
        }

        let Ok(file_names) = std::fs::read_dir(entry.path()) else {
            continue;
        };
        for file_entry in file_names.flatten() {
            let file_name = file_entry.file_name().to_string_lossy().into_owned();
            if is_transcript_of(&file_name, &session_ids) {
                newest = newest_ms([newest, mtime_ms(&file_entry.path())]);
            }
        }
    }

    newest
}

fn is_transcript_of(file_name: &str, session_ids: &[String]) -> bool {
    let Some(stem) = file_name.strip_suffix(SESSION_TRANSCRIPT_SUFFIX) else {
        return false;
    };

    session_ids.iter().any(|session_id| {
        stem == session_id.as_str()
            || stem
                .strip_suffix(session_id.as_str())
                .is_some_and(|head| head.ends_with('_'))
    })
}

/// The encoding of a session cwd into a directory name belongs to the agent, so the two are
/// compared on their alphanumeric shape and in both directions: a session cwd and the path the
/// agent recorded can differ by a resolved symlink prefix such as /var vs /private/var.
fn matches_session_cwd(directory_name: &str, needles: &[String]) -> bool {
    let normalized = normalize_path_for_match(directory_name);
    needles
        .iter()
        .any(|needle| normalized.contains(needle.as_str()) || needle.contains(&normalized))
}

fn normalize_path_for_match(value: &str) -> String {
    let lowered = value.to_lowercase();
    let mut normalized = String::with_capacity(lowered.len());
    for character in lowered.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            normalized.push(character);
        } else if !normalized.ends_with('-') {
            normalized.push('-');
        }
    }
    normalized
}

fn mtime_ms(path: &Path) -> Option<f64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64() * 1_000.0,
        Err(error) => -error.duration().as_secs_f64() * 1_000.0,
    })
}

fn newest_ms(values: impl IntoIterator<Item = Option<f64>>) -> Option<f64> {
    let mut newest: Option<f64> = None;
    for value in values {
        let Some(value) = value else {
            continue;
        };
        if newest.is_none_or(|current| value > current) {
            newest = Some(value);
        }
    }
    newest
}

