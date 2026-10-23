//! Pull channel: an RPC client writes reviewed facts through the facts extractor's own apply (pin
//! `facts-apply-rpc.ts`, `facts-apply-runtime.ts`, and the applied-envelope half of
//! `facts-batch-apply.ts`).
//!
//! The batch's recovery envelope lives in `<facts>/applied/<batchId>/`: removed once the apply reaches
//! a terminal outcome, kept whenever it ends otherwise (an error, or a busy memory-write lock) with a
//! planned recovery on disk, so [`crate::facts_applied_reconcile`] or a retry with the same batch id
//! finishes it.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use memory_core::agreements::{AgreementExtractionRecord, AgreementsPolicy};
use memory_core::facts::extraction::{FactsExtractionRecord, parse_facts_extraction_record};
use memory_core::facts::person_routing::FactsPeopleRouting;
use memory_core::git::GitMemoryRepo;
use memory_core::identity::resolve::{MemoryIdentity, sanitize_to_slug};
use memory_core::locks::{
    AcquireLockOptions, CreateLockRecordOptions, WithLockError, create_lock_record,
    get_process_start_identity, memory_writer_lock_path, with_lock,
};
use serde::{Deserialize, Serialize};

use crate::context::MemoryIdentityContext;
use crate::facts_batch_apply::{
    Applied, apply_claimed, apply_facts_with_retries, resolve_facts_people_routing,
};
use crate::facts_runner_types::FactsRunLedger;
use crate::worker::run_artifacts::{read_run_json, write_run_json_atomic};

pub const MEMORY_FACTS_APPLY_RPC_METHOD: &str = "omo.memory.facts.apply";

/// What an applied batch's envelope keeps so a later process can finish it (`request.json`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppliedFactsRequest {
    pub records: Vec<FactsExtractionRecord>,
    pub people: FactsPeopleRouting,
}

pub fn applied_facts_dir(facts_dir: &Path) -> PathBuf {
    facts_dir.join("applied")
}

/// Envelopes this process is applying right now.
pub fn active_applied_batches() -> &'static Mutex<HashSet<PathBuf>> {
    static ACTIVE: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    ACTIVE.get_or_init(Mutex::default)
}

/// The writer lock the applier takes around one attempt: the cross-process `memory-write` lock, 2 s
/// wait per attempt. The closure runs inside it.
pub type WriterLock<'a> = &'a dyn Fn(
    usize,
    &mut dyn FnMut() -> Result<Applied, String>,
) -> Result<Applied, WithLockError<String>>;

/// One writer-locked attempt; `WriterLock` borrows it.
pub type ApplyAttempt<'a> = &'a mut dyn FnMut() -> Result<Applied, String>;

pub fn facts_writer_lock(
    locks_dir: PathBuf,
    run_id: String,
) -> impl Fn(usize, ApplyAttempt<'_>) -> Result<Applied, WithLockError<String>> {
    move |_attempt, operation| {
        let record = create_lock_record(
            "memory-write",
            CreateLockRecordOptions {
                run_id: Some(run_id.clone()),
            },
        )
        .map_err(|error| WithLockError::User(format!("{error:?}")))?;
        with_lock(
            &memory_writer_lock_path(&locks_dir),
            &record,
            &AcquireLockOptions {
                wait_timeout_ms: Some(2_000),
                ..Default::default()
            },
            operation,
        )
    }
}

/// `apply_facts_with_retries` over the claimed apply: three attempts on writer-lock contention with
/// jittered delay; `None` when the lock stayed busy. Applied envelopes carry no agreement records.
pub fn apply_envelope(
    run_dir: &Path,
    ledger: &FactsRunLedger,
    repo: &GitMemoryRepo,
    records: &[FactsExtractionRecord],
    people: &FactsPeopleRouting,
    identity: &str,
    lock: WriterLock<'_>,
) -> Result<Option<Applied>, String> {
    apply_envelope_with(
        run_dir,
        ledger,
        repo,
        records,
        &[],
        &AgreementsPolicy::default(),
        people,
        identity,
        lock,
        &|_| {},
    )
}

/// [`apply_envelope`] with the agreement records, their policy, and a warning sink (upstream
/// `applyFactsWithRetries` options `agreements`, `agreementsPolicy`, `logger`).
#[allow(clippy::too_many_arguments)]
pub fn apply_envelope_with(
    run_dir: &Path,
    ledger: &FactsRunLedger,
    repo: &GitMemoryRepo,
    records: &[FactsExtractionRecord],
    agreements: &[AgreementExtractionRecord],
    agreements_policy: &AgreementsPolicy,
    people: &FactsPeopleRouting,
    identity: &str,
    lock: WriterLock<'_>,
    warn: &dyn Fn(&str),
) -> Result<Option<Applied>, String> {
    apply_facts_with_retries(
        |attempt| {
            lock(attempt, &mut || {
                apply_claimed(
                    run_dir,
                    ledger,
                    repo,
                    records,
                    agreements,
                    agreements_policy,
                    people,
                    identity,
                    &mut |message| warn(message),
                )
                .map_err(|error| error.message())
            })
        },
        |_, delay| std::thread::sleep(std::time::Duration::from_millis(delay)),
        || {
            let id = memory_core::support::random::random_id();
            let value = id.bytes().take(8).fold(0u32, |value, byte| {
                value * 16
                    + u32::from(if byte <= b'9' {
                        byte - b'0'
                    } else {
                        byte.saturating_sub(b'a') + 10
                    })
            });
            f64::from(value) / (f64::from(u32::MAX) + 1.0)
        },
    )
    .map_err(|error| error.to_string())
}

/// What the RPC reports for a finished apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppliedWithPaths {
    Committed {
        sha: String,
        affected_paths: Vec<String>,
    },
    ParentDirty {
        detail: Option<String>,
    },
    Unchanged,
}

/// Applies records that did not come from a queued extractor run (a reviewed onboarding selection)
/// through the same path: same lock, same person routing, same batch receipt. Blocking.
pub fn apply_facts_under_lock(
    identity: &MemoryIdentity,
    batch_id: &str,
    records: &[FactsExtractionRecord],
    people: &FactsPeopleRouting,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<AppliedWithPaths>, String> {
    crate::context::ensure_identity_runtime_dirs(&identity.paths)
        .map_err(|error| error.to_string())?;
    let repo = GitMemoryRepo::open(&identity.paths.repo, &identity.id)
        .map_err(|error| error.to_string())?;
    if !identity.paths.repo.join(".git").exists() {
        repo.init(memory_core::git::InitializeGitRepoOptions {
            seed_files: memory_core::seeds::seeds::build_default_seed_files()
                .into_iter()
                .map(|seed| memory_core::git::GitSeedFile {
                    relative_path: seed.relative_path,
                    content: seed.content,
                })
                .collect(),
            ..Default::default()
        })
        .map_err(|error| error.to_string())?;
    }
    let run_dir = applied_facts_dir(&identity.paths.facts).join(batch_id);
    create_private_dir(&run_dir).map_err(|error| error.to_string())?;
    let ledger_path = run_dir.join("ledger.json");
    let kept: Option<FactsRunLedger> = read_run_json(&ledger_path).ok();
    let resumed = kept
        .as_ref()
        .is_some_and(|kept| kept.batch_id == batch_id && kept.apply_recovery.is_some());
    let mut ledger = match kept.filter(|_| resumed) {
        Some(kept) => kept,
        None => serde_json::from_value(serde_json::json!({
            "version": 1, "runId": format!("apply-{batch_id}"), "kind": "facts", "startedAt": now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "hardDeadlineAt": now.timestamp_millis(), "terminationGraceMs": 0, "deadlineAt": now.timestamp_millis(), "batchId": batch_id, "queued": [],
        }))
        .map_err(|error| error.to_string())?,
    };
    ledger.pid = Some(std::process::id());
    ledger.process_start = get_process_start_identity(std::process::id());
    active_applied_batches()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(run_dir.clone());
    let result = (|| -> Result<Option<Applied>, String> {
        if !resumed {
            write_run_json_atomic(
                &run_dir.join("request.json"),
                &AppliedFactsRequest {
                    records: records.to_vec(),
                    people: *people,
                },
                0o600,
            )
            .map_err(|error| error.to_string())?;
        }
        write_run_json_atomic(&ledger_path, &ledger, 0o600).map_err(|error| error.to_string())?;
        let lock = facts_writer_lock(identity.paths.locks.clone(), ledger.run_id.clone());
        apply_envelope(
            &run_dir,
            &ledger,
            &repo,
            records,
            people,
            &identity.id,
            &lock,
        )
    })();
    // An error or an exhausted lock is not terminal: the on-disk ledger, not this call's path, says
    // whether a planned recovery exists. Kept envelopes are unowned, so any later reconcile finishes them.
    let settled = matches!(result, Ok(Some(_)));
    let on_disk: Option<serde_json::Map<String, serde_json::Value>> = if settled {
        None
    } else {
        read_run_json(&ledger_path).ok()
    };
    let keep = on_disk.as_ref().is_some_and(|ledger| {
        ledger
            .get("applyRecovery")
            .is_some_and(|recovery| !recovery.is_null())
    });
    if let Some(mut ledger) = on_disk.filter(|_| keep) {
        ledger.remove("pid");
        ledger.remove("processStart");
        let _ = write_run_json_atomic(&ledger_path, &ledger, 0o600);
    }
    active_applied_batches()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&run_dir);
    if !keep {
        let _ = std::fs::remove_dir_all(&run_dir);
    }
    Ok(match result? {
        None => None,
        Some(Applied::ParentDirty { detail }) => Some(AppliedWithPaths::ParentDirty { detail }),
        Some(Applied::Unchanged) => Some(AppliedWithPaths::Unchanged),
        Some(Applied::Committed {
            sha,
            affected_paths,
        }) => Some(AppliedWithPaths::Committed {
            sha,
            affected_paths,
        }),
    })
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// memory-core's recovery accepts only UUID v4 batch ids; the id also names the envelope directory.
fn is_batch_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    groups.len() == 5
        && groups.iter().zip(lengths).all(|(group, length)| {
            group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        && groups[2].starts_with('4')
        && groups[3]
            .bytes()
            .next()
            .is_some_and(|byte| matches!(byte.to_ascii_lowercase(), b'8' | b'9' | b'a' | b'b'))
}

/// One apply call: the batch id, the validated records, and the people routing.
pub struct FactsApplyCall {
    pub batch_id: String,
    pub records: Vec<FactsExtractionRecord>,
    pub people: FactsPeopleRouting,
}

pub type FactsApplyFn<'a> =
    &'a (dyn Fn(FactsApplyCall) -> Result<Option<AppliedWithPaths>, String> + Sync);

/// Validates the request, then hands the batch to the extractor's apply; unselected records never
/// arrive here. Returns the RPC response.
pub fn apply_memory_facts(
    people: FactsPeopleRouting,
    request: &serde_json::Value,
    new_batch_id: &dyn Fn() -> String,
    apply: FactsApplyFn<'_>,
) -> Result<serde_json::Value, String> {
    let invalid = |reason: String| Ok(serde_json::json!({"kind": "invalid", "reason": reason}));
    let Some(raw) = request.get("records").and_then(serde_json::Value::as_array) else {
        return invalid("facts apply needs a records list".into());
    };
    let client_batch_id = match request.get("batchId") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => match value.as_str().filter(|id| is_batch_id(id)) {
            Some(id) => Some(id.to_owned()),
            None => return invalid("batchId must be a UUID v4".into()),
        },
    };
    let mut records = Vec::with_capacity(raw.len());
    for (index, value) in raw.iter().enumerate() {
        match parse_facts_extraction_record(value, index) {
            Ok(record) => records.push(record),
            Err(error) => {
                return invalid(format!("record {} rejected: {}", index + 1, error.message));
            }
        }
    }
    if records.is_empty() {
        return Ok(serde_json::json!({"outcome": "no_facts", "affectedPaths": []}));
    }
    let batch_id = client_batch_id.unwrap_or_else(new_batch_id);
    let applied = match apply(FactsApplyCall {
        batch_id: batch_id.clone(),
        records,
        people,
    }) {
        Ok(applied) => applied,
        Err(error) => {
            return match crate::facts_batch_apply::secret_rejection_reason(&error) {
                Some(reason) => invalid(reason.to_owned()),
                None => Err(error),
            };
        }
    };
    match applied {
        None => Ok(
            serde_json::json!({"kind": "unavailable", "reason": "The memory-write lock stayed busy; try again."}),
        ),
        Some(AppliedWithPaths::ParentDirty { detail }) => {
            let mut response = serde_json::json!({"outcome": "parent_dirty", "batchId": batch_id});
            if let Some(detail) = detail {
                response["detail"] = serde_json::Value::String(detail);
            }
            Ok(response)
        }
        Some(AppliedWithPaths::Unchanged) => {
            Ok(serde_json::json!({"outcome": "no_facts", "affectedPaths": []}))
        }
        Some(AppliedWithPaths::Committed {
            sha,
            affected_paths,
        }) => Ok(
            serde_json::json!({"outcome": "committed", "sha": sha, "affectedPaths": affected_paths, "batchId": batch_id}),
        ),
    }
}

pub type FactsApplierFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>;
/// `(request, session_id)` -> response.
pub type MemoryFactsApplier =
    Arc<dyn Fn(serde_json::Value, Option<String>) -> FactsApplierFuture + Send + Sync>;

pub type ResolveIdentityContext =
    Arc<dyn Fn(Option<&str>) -> Option<MemoryIdentityContext> + Send + Sync>;

/// The applier the memory component owns: the session's bound identity and its configured people routing.
pub fn create_session_facts_applier(
    resolve_context: ResolveIdentityContext,
    people: Arc<dyn Fn(&str) -> FactsPeopleRouting + Send + Sync>,
) -> MemoryFactsApplier {
    Arc::new(move |request, session_id| {
        let resolve_context = resolve_context.clone();
        let people = people.clone();
        Box::pin(async move {
            let Some(context) = resolve_context(session_id.as_deref()) else {
                return Ok(
                    serde_json::json!({"kind": "unavailable", "reason": "No bound memory session."}),
                );
            };
            let routing = people(&context.identity);
            let identity = MemoryIdentity {
                id: context.identity.clone(),
                safe_slug: sanitize_to_slug(&context.identity),
                paths: context.identity_paths.clone(),
            };
            tokio::task::spawn_blocking(move || {
                apply_memory_facts(
                    routing,
                    &request,
                    &memory_core::support::random::random_uuid,
                    &|call| {
                        apply_facts_under_lock(
                            &identity,
                            &call.batch_id,
                            &call.records,
                            &call.people,
                            chrono::Utc::now(),
                        )
                    },
                )
            })
            .await
            .map_err(|error| format!("The memory apply runtime could not load: {error}"))?
        })
    })
}

/// The session applier with people routing read from the current omo config on every call.
pub fn create_configured_facts_applier(
    load_config: Arc<dyn Fn() -> serde_json::Value + Send + Sync>,
    resolve_context: ResolveIdentityContext,
) -> MemoryFactsApplier {
    create_session_facts_applier(
        resolve_context,
        Arc::new(move |identity| resolve_facts_people_routing(&load_config(), identity)),
    )
}

// The onboarding component is registered before memory and cannot import its wiring, so the memory
// component publishes its one applier here, process-wide.
fn applier_slot() -> &'static Mutex<Option<MemoryFactsApplier>> {
    static SLOT: OnceLock<Mutex<Option<MemoryFactsApplier>>> = OnceLock::new();
    SLOT.get_or_init(Mutex::default)
}

pub fn publish_session_facts_applier(applier: Option<MemoryFactsApplier>) {
    *applier_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = applier;
}

pub fn session_facts_applier() -> Option<MemoryFactsApplier> {
    applier_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BATCH: &str = "c04edbea-90ea-4c67-9c22-1a9beb207535";

    fn routing() -> FactsPeopleRouting {
        FactsPeopleRouting {
            enabled: true,
            max_entries: 40,
            max_entry_chars: 200,
        }
    }

    fn call(request: serde_json::Value, apply: FactsApplyFn<'_>) -> serde_json::Value {
        apply_memory_facts(routing(), &request, &|| BATCH.to_owned(), apply).unwrap()
    }

    #[test]
    fn a_request_without_records_is_invalid() {
        let response = call(serde_json::json!({}), &|_| panic!("must not apply"));
        assert_eq!(
            response,
            serde_json::json!({"kind": "invalid", "reason": "facts apply needs a records list"})
        );
    }

    #[test]
    fn a_non_uuid_batch_id_is_invalid_and_a_v4_one_is_kept() {
        let bad = call(
            serde_json::json!({"records": [], "batchId": "nope"}),
            &|_| panic!("must not apply"),
        );
        assert_eq!(bad["reason"], "batchId must be a UUID v4");
        assert!(is_batch_id(BATCH));
        assert!(!is_batch_id("c04edbea-90ea-1c67-9c22-1a9beb207535"));
    }

    #[test]
    fn an_empty_list_is_no_facts_and_a_bad_record_names_its_position() {
        assert_eq!(
            call(serde_json::json!({"records": []}), &|_| panic!(
                "must not apply"
            ))["outcome"],
            "no_facts"
        );
        let rejected = call(
            serde_json::json!({"records": [{"scope": "project", "text": "ok fact", "date": "2026-10-01"}, {"scope": "nope"}]}),
            &|_| panic!("must not apply"),
        );
        assert_eq!(rejected["kind"], "invalid");
        assert!(
            rejected["reason"]
                .as_str()
                .unwrap()
                .starts_with("record 2 rejected: ")
        );
    }

    #[test]
    fn apply_outcomes_map_to_wire_responses() {
        let request = serde_json::json!({"records": [{"scope": "project", "text": "ok fact", "date": "2026-10-01"}]});
        let committed = call(request.clone(), &|call| {
            assert_eq!(call.batch_id, BATCH);
            Ok(Some(AppliedWithPaths::Committed {
                sha: "abc".into(),
                affected_paths: vec!["notes/a.md".into()],
            }))
        });
        assert_eq!(
            committed,
            serde_json::json!({"outcome": "committed", "sha": "abc", "affectedPaths": ["notes/a.md"], "batchId": BATCH})
        );
        let dirty = call(request.clone(), &|_| {
            Ok(Some(AppliedWithPaths::ParentDirty {
                detail: Some("foreign".into()),
            }))
        });
        assert_eq!(
            dirty,
            serde_json::json!({"outcome": "parent_dirty", "batchId": BATCH, "detail": "foreign"})
        );
        let busy = call(request, &|_| Ok(None));
        assert_eq!(busy["kind"], "unavailable");
    }

    #[tokio::test]
    async fn an_unbound_session_is_unavailable() {
        let applier = create_session_facts_applier(Arc::new(|_| None), Arc::new(|_| routing()));
        let response = applier(serde_json::json!({}), None).await.unwrap();
        assert_eq!(
            response,
            serde_json::json!({"kind": "unavailable", "reason": "No bound memory session."})
        );
    }

    #[test]
    fn the_published_applier_is_process_wide() {
        publish_session_facts_applier(None);
        assert!(session_facts_applier().is_none());
        publish_session_facts_applier(Some(create_session_facts_applier(
            Arc::new(|_| None),
            Arc::new(|_| routing()),
        )));
        assert!(session_facts_applier().is_some());
        publish_session_facts_applier(None);
    }
}
