use crate::{
    facts_runner_types::FactsRunLedger,
    worker::run_artifacts::{ArtifactError, update_run_ledger},
};
use memory_core::{
    agreements::{AgreementEvent, AgreementExtractionRecord, AgreementsPolicy},
    facts::{
        extraction::{
            FactsBatch, FactsExtractionError, FactsExtractionRecord, validate_facts_recovery,
        },
        mutation_plan::{MutationPlanError, plan_facts_mutation},
        person_routing::FactsPeopleRouting,
        recovery::{FactsRecoveryResult, find_facts_batch_receipt},
    },
    git::{GitCommitAuthor, GitLogOptions, GitMemoryRepo, errors::GitError},
};
use std::path::Path;
#[derive(Debug)]
pub enum FactsApplyError {
    Git(GitError),
    Plan(MutationPlanError),
    Extraction(FactsExtractionError),
    Recovery(memory_core::facts::recovery::FactsRecoveryError),
    Artifact(ArtifactError),
    Secret(String),
}
/// Prefix that carries a secret rejection (upstream `MemorySecretError`) through string-typed apply errors.
pub const SECRET_REJECTION_PREFIX: &str = "memory secret rejected: ";
pub fn secret_rejection_reason(error: &str) -> Option<&str> {
    error.strip_prefix(SECRET_REJECTION_PREFIX)
}
impl FactsApplyError {
    pub fn message(&self) -> String {
        match self {
            Self::Secret(reason) => format!("{SECRET_REJECTION_PREFIX}{reason}"),
            other => format!("{other:?}"),
        }
    }
}
fn secret_like_record(
    records: &[FactsExtractionRecord],
    agreements: &[AgreementExtractionRecord],
) -> Option<String> {
    use memory_core::sync::contains_secret_like_material as secret;
    if records.iter().any(|record| secret(record.text())) {
        return Some("facts record text matches a secret pattern".into());
    }
    agreements
        .iter()
        .any(|record| {
            serde_json::to_value(record)
                .ok()
                .and_then(|value| value.get("text").and_then(|text| text.as_str().map(secret)))
                == Some(true)
        })
        .then(|| "agreement record text matches a secret pattern".into())
}
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    Committed {
        sha: String,
        affected_paths: Vec<String>,
    },
    ParentDirty {
        detail: Option<String>,
    },
    Unchanged,
}
/// Logs an agreement-ledger event the plan raised (upstream `logAgreementEvent`).
pub fn log_agreement_event(warn: &mut dyn FnMut(&str), batch_id: &str, event: &AgreementEvent) {
    match event {
        AgreementEvent::Evicted { item, for_text, .. } => warn(&format!(
            "agreement ledger full: evicted the oldest unreaffirmed agreement: batchId={batch_id} id={} text={} since={} forText={for_text}",
            item.id,
            item.text,
            item.since.as_deref().unwrap_or("")
        )),
        AgreementEvent::Refused { text, .. } => warn(&format!(
            "agreement ledger full: refused a new agreement (nothing evictable): batchId={batch_id} text={text}"
        )),
        AgreementEvent::StaleReaffirm { id, .. } => warn(&format!(
            "agreement reaffirm ignored: the agreement is no longer in the ledger: batchId={batch_id} id={id}"
        )),
    }
}
/// A batch answered from its receipt reports the paths that commit wrote (upstream `receiptPaths`).
fn receipt_paths(repo: &GitMemoryRepo, sha: &str, batch_id: &str) -> Vec<String> {
    repo.log(Some(&GitLogOptions {
        include_paths: true,
        grep: Some(format!("Omo-Facts-Batch: {batch_id}")),
        ..Default::default()
    }))
    .ok()
    .and_then(|commits| commits.into_iter().find(|commit| commit.sha == sha))
    .and_then(|commit| commit.paths)
    .unwrap_or_default()
}
pub fn resolve_facts_people_routing(
    config: &serde_json::Value,
    identity: &str,
) -> FactsPeopleRouting {
    let memory = &config["memory"];
    let overrides = &memory["agents"][identity]["people"];
    let people = &memory["people"];
    // SAFETY-DIVERGENCE(packages/omo-config-core/src/schema/memory.ts): upstream defaults memory.people.enabled to true; mhc defaults to false.
    FactsPeopleRouting {
        enabled: overrides["enabled"]
            .as_bool()
            .or_else(|| people["enabled"].as_bool())
            .unwrap_or(false),
        max_entries: overrides["max_entries"]
            .as_u64()
            .or_else(|| people["max_entries"].as_u64())
            .unwrap_or(40) as usize,
        max_entry_chars: overrides["max_entry_chars"]
            .as_u64()
            .or_else(|| people["max_entry_chars"].as_u64())
            .unwrap_or(200) as usize,
    }
}
pub fn apply_facts_with_retries<T, E>(
    mut operation: impl FnMut(usize) -> Result<T, memory_core::locks::WithLockError<E>>,
    mut retry_delay: impl FnMut(usize, u64),
    mut random: impl FnMut() -> f64,
) -> Result<Option<T>, memory_core::locks::WithLockError<E>> {
    for attempt in 1..=3 {
        match operation(attempt) {
            Ok(applied) => return Ok(Some(applied)),
            Err(memory_core::locks::WithLockError::Acquire(
                memory_core::locks::AcquireLockError::Contention(_),
            )) => {
                if attempt == 3 {
                    return Ok(None);
                }
                retry_delay(attempt, 25 + (random() * 76.0).floor() as u64);
            }
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}
#[allow(clippy::too_many_arguments)]
pub fn apply_claimed(
    run_dir: &Path,
    ledger: &FactsRunLedger,
    repo: &GitMemoryRepo,
    records: &[FactsExtractionRecord],
    agreements: &[AgreementExtractionRecord],
    agreements_policy: &AgreementsPolicy,
    people: &FactsPeopleRouting,
    identity: &str,
    warn: &mut dyn FnMut(&str),
) -> Result<Applied, FactsApplyError> {
    if let Some(receipt) =
        find_facts_batch_receipt(repo, &ledger.batch_id).map_err(FactsApplyError::Git)?
    {
        let affected_paths = receipt_paths(repo, &receipt.sha, &ledger.batch_id);
        return Ok(Applied::Committed {
            sha: receipt.sha,
            affected_paths,
        });
    }
    let batch = FactsBatch {
        batch_id: ledger.batch_id.clone(),
        records: records.to_vec(),
        agreements: (!agreements.is_empty()).then(|| agreements.to_vec()),
    };
    if ledger.apply_recovery.is_none()
        && let Some(reason) = secret_like_record(records, agreements)
    {
        return Err(FactsApplyError::Secret(reason));
    }
    let recovery = if let Some(recovery) = &ledger.apply_recovery {
        validate_facts_recovery(recovery, &batch).map_err(FactsApplyError::Extraction)?;
        recovery.clone()
    } else {
        let mut on_event =
            |event: &AgreementEvent| log_agreement_event(warn, &ledger.batch_id, event);
        let recovery = match plan_facts_mutation(
            repo,
            &batch,
            Some(people),
            None,
            agreements_policy,
            Some(&mut on_event),
        ) {
            Ok(recovery) => recovery,
            Err(MutationPlanError::ParentDirty(_)) => {
                return Ok(Applied::ParentDirty { detail: None });
            }
            Err(error) => return Err(FactsApplyError::Plan(error)),
        };
        if recovery.paths.is_empty() {
            return Ok(Applied::Unchanged);
        }
        let fields = serde_json::Map::from_iter([
            (
                "headBeforeApply".into(),
                serde_json::Value::String(recovery.head_before_apply.clone()),
            ),
            (
                "applyRecovery".into(),
                serde_json::to_value(&recovery)
                    .map_err(|error| FactsApplyError::Artifact(ArtifactError::Json(error)))?,
            ),
        ]);
        update_run_ledger(&run_dir.join("ledger.json"), &fields)
            .map_err(FactsApplyError::Artifact)?;
        recovery
    };
    let mut publish = |effective: &memory_core::facts::mutation_plan::FactsApplyRecovery| {
        let fields = serde_json::Map::from_iter([("applyRecovery".into(), serde_json::to_value(effective).map_err(|error| error.to_string())?)]);
        update_run_ledger(&run_dir.join("ledger.json"), &fields).map_err(|error| error.to_string())
    };
    match memory_core::facts::recovery::apply_facts_recovery_with_agreements(
        repo,
        &recovery,
        records.len() + agreements.len(),
        &GitCommitAuthor {
            agent_id: identity.into(),
            author_name: "Facts Extractor".into(),
            author_email: None,
        },
        Some(memory_core::facts::recovery::AgreementRecoveryInput { batch: &batch, policy: agreements_policy, publish: &mut publish }),
    )
    .map_err(FactsApplyError::Recovery)?
    {
        FactsRecoveryResult::Committed {
            sha,
            affected_paths,
        } => Ok(Applied::Committed {
            sha,
            affected_paths,
        }),
        FactsRecoveryResult::ParentDirty { detail } => Ok(Applied::ParentDirty { detail }),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn people_routing_resolves_agent_then_global_then_defaults() {
        let config = serde_json::json!({"memory":{"people":{"enabled":true,"max_entries":17},"agents":{"agent":{"people":{"enabled":false,"max_entry_chars":91}}}}});
        assert_eq!(
            resolve_facts_people_routing(&config, "agent"),
            FactsPeopleRouting {
                enabled: false,
                max_entries: 17,
                max_entry_chars: 91
            }
        );
        assert_eq!(
            resolve_facts_people_routing(&config, "other"),
            FactsPeopleRouting {
                enabled: true,
                max_entries: 17,
                max_entry_chars: 200
            }
        );
        assert_eq!(
            resolve_facts_people_routing(&serde_json::json!({}), "agent"),
            FactsPeopleRouting {
                enabled: false,
                max_entries: 40,
                max_entry_chars: 200
            }
        );
    }
    fn contention() -> memory_core::locks::WithLockError<String> {
        memory_core::locks::WithLockError::Acquire(
            memory_core::locks::AcquireLockError::Contention(Box::new(
                memory_core::locks::LockContentionError::new("writer.lock".into(), None),
            )),
        )
    }
    #[test]
    fn contention_retries_with_injected_jitter_before_success() {
        let mut attempts = vec![];
        let mut delays = vec![];
        let result = apply_facts_with_retries(
            |attempt| {
                attempts.push(attempt);
                if attempt < 3 {
                    Err(contention())
                } else {
                    Ok("committed")
                }
            },
            |attempt, delay| delays.push((attempt, delay)),
            || 0.5,
        )
        .unwrap();
        assert_eq!(result, Some("committed"));
        assert_eq!(attempts, [1, 2, 3]);
        assert_eq!(delays, [(1, 63), (2, 63)]);
    }
    #[test]
    fn contention_exhaustion_stops_after_three_attempts() {
        let mut attempts = 0;
        let mut delays = 0;
        let result = apply_facts_with_retries::<(), String>(
            |_| {
                attempts += 1;
                Err(contention())
            },
            |_, _| delays += 1,
            || 0.0,
        )
        .unwrap();
        assert_eq!(result, None);
        assert_eq!(attempts, 3);
        assert_eq!(delays, 2);
    }
    #[test]
    fn noncontention_error_never_retries() {
        let error = apply_facts_with_retries::<(), String>(
            |_| {
                Err(memory_core::locks::WithLockError::Acquire(
                    memory_core::locks::AcquireLockError::Aborted,
                ))
            },
            |_, _| panic!("must not delay"),
            || panic!("must not sample jitter"),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            memory_core::locks::WithLockError::Acquire(
                memory_core::locks::AcquireLockError::Aborted
            )
        ));
    }
    fn ledger() -> FactsRunLedger {
        serde_json::from_value(serde_json::json!({"version":1,"runId":"run","kind":"facts","startedAt":"now","hardDeadlineAt":1,"terminationGraceMs":1,"deadlineAt":2,"batchId":"c04edbea-90ea-4c67-9c22-1a9beb207535","queued":[]})).unwrap()
    }
    #[test]
    fn durable_recovery_precedes_commit_and_receipt_makes_replay_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let engine = crate::engine_session::prepare_memory_engine_session(
            "agent",
            &paths,
            Default::default(),
        )
        .unwrap();
        let run = root.path().join("run");
        std::fs::create_dir(&run).unwrap();
        let ledger = ledger();
        crate::worker::run_artifacts::write_run_json_atomic(
            &run.join("ledger.json"),
            &ledger,
            0o600,
        )
        .unwrap();
        let records = vec![FactsExtractionRecord::Project {
            receipt: None,
            text: "Project uses a durable journal".into(),
            date: "2026-08-12".into(),
        }];
        let people = FactsPeopleRouting {
            enabled: true,
            max_entries: 40,
            max_entry_chars: 200,
        };
        let first = apply_claimed(
            &run,
            &ledger,
            &engine.repo,
            &records,
            &[],
            &AgreementsPolicy::default(),
            &people,
            "agent",
            &mut |_| {},
        )
        .unwrap();
        let persisted: FactsRunLedger =
            crate::worker::run_artifacts::read_run_json(&run.join("ledger.json")).unwrap();
        assert!(persisted.apply_recovery.is_some());
        assert!(persisted.head_before_apply.is_some());
        let sha = engine.repo.head().unwrap();
        let second = apply_claimed(
            &run,
            &persisted,
            &engine.repo,
            &records,
            &[],
            &AgreementsPolicy::default(),
            &people,
            "agent",
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(engine.repo.head().unwrap(), sha);
        assert!(
            engine
                .repo
                .status(&[] as &[&str])
                .unwrap()
                .trim()
                .is_empty()
        );
    }
    #[test]
    fn legacy_agreement_recovery_is_published_before_commit() {
        let root=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");
        let engine=crate::engine_session::prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();
        let run=root.path().join("run");std::fs::create_dir(&run).unwrap();let mut ledger=ledger();
        let agreements=vec![memory_core::agreements::parse_agreement_record(&serde_json::json!({"action":"new","applies":"global","text":"Use fixtures","date":"2026-10-01","source":{"sessionId":"session","messageId":"user"}}),0).unwrap()];
        let batch=FactsBatch{batch_id:ledger.batch_id.clone(),records:vec![],agreements:Some(agreements.clone())};
        let policy=AgreementsPolicy::default();let mut recovery=plan_facts_mutation(&engine.repo,&batch,None,None,&policy,None).unwrap();recovery.agreement_event_version=None;ledger.apply_recovery=Some(recovery);
        crate::worker::run_artifacts::write_run_json_atomic(&run.join("ledger.json"),&ledger,0o600).unwrap();
        let people=FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200};
        assert!(matches!(apply_claimed(&run,&ledger,&engine.repo,&[],&agreements,&policy,&people,"agent",&mut |_|{}).unwrap(),Applied::Committed{..}));
        let persisted:FactsRunLedger=crate::worker::run_artifacts::read_run_json(&run.join("ledger.json")).unwrap();assert_eq!(persisted.apply_recovery.unwrap().agreement_event_version,Some(1));
    }
    #[test]
    fn foreign_dirty_worktree_is_retained() {
        let root = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
        let engine = crate::engine_session::prepare_memory_engine_session(
            "agent",
            &paths,
            Default::default(),
        )
        .unwrap();
        let foreign = paths.repo.join("foreign.txt");
        std::fs::write(&foreign, "user change").unwrap();
        let records = vec![FactsExtractionRecord::Project {
            receipt: None,
            text: "fact".into(),
            date: "2026-08-12".into(),
        }];
        let people = FactsPeopleRouting {
            enabled: true,
            max_entries: 40,
            max_entry_chars: 200,
        };
        assert_eq!(
            apply_claimed(
                root.path(),
                &ledger(),
                &engine.repo,
                &records,
                &[],
                &AgreementsPolicy::default(),
                &people,
                "agent",
                &mut |_| {}
            )
            .unwrap(),
            Applied::ParentDirty { detail: None }
        );
        assert_eq!(std::fs::read_to_string(foreign).unwrap(), "user change");
        assert!(!root.path().join("ledger.json").exists());
    }
}
