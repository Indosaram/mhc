//! Batch steering application.

use std::path::Path;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain_types::{UlwLoopLedgerEntry, UlwLoopPlan};
use crate::paths::UlwLoopScope;
use crate::plan_commit::{commit, CommitMutation};
use crate::plan_io::{find_accepted_steering_entry, read_ledger, read_ulw_loop_plan, with_ulw_loop_mutation_lock};
use crate::runtime::{iso, UlwLoopError};
use crate::steering::{apply_steering_mutation, validate_ulw_loop_steering_proposal};
use crate::steering_snapshot::{build_steering_plan_snapshot, changed_goal_ids_between};
use crate::steering_types::{UlwLoopSteeringAudit, UlwLoopSteeringProposal};
use crate::validation_batch::batch_update_ledger_entry;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SteerUlwLoopBatchItemResult {
    pub accepted: bool,
    pub deduped: bool,
    pub audit: UlwLoopSteeringAudit,
    #[serde(rename = "rejectedReasons")]
    pub rejected_reasons: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SteerUlwLoopBatchResult {
    pub plan: UlwLoopPlan,
    pub accepted: bool,
    pub results: Vec<SteerUlwLoopBatchItemResult>,
    #[serde(rename = "rejectedReasons")]
    pub rejected_reasons: Vec<String>,
}

struct PreparedFresh {
    proposal: UlwLoopSteeringProposal,
    audit: UlwLoopSteeringAudit,
    next: UlwLoopPlan,
}

enum PreparedItem {
    Deduped(SteerUlwLoopBatchItemResult),
    Fresh(PreparedFresh),
}

fn rejected_ledger_entry(results: &[SteerUlwLoopBatchItemResult]) -> UlwLoopLedgerEntry {
    let mut messages = Vec::new();
    for (i, r) in results.iter().enumerate() {
        if !r.accepted {
            messages.push(format!("index {i}: {}", r.rejected_reasons.join(", ")));
        }
    }
    UlwLoopLedgerEntry {
        at: iso(),
        kind: "steering_rejected".to_string(),
        message: Some(messages.join("; ")),
        ..Default::default()
    }
}

fn item_ledger_entry(proposal: &UlwLoopSteeringProposal, audit: &UlwLoopSteeringAudit, at: &str) -> UlwLoopLedgerEntry {
    let kind = if proposal.kind == "revise_criterion" {
        "criteria_revised".to_string()
    } else {
        "steering_accepted".to_string()
    };
    UlwLoopLedgerEntry {
        at: at.to_string(),
        kind,
        evidence: Some(proposal.evidence.clone()),
        message: Some(proposal.rationale.clone()),
        steering: Some(audit.clone()),
        mutation_kind: Some(proposal.kind.clone()),
        goal_id: audit.target_goal_ids.first().cloned(),
        criterion_id: proposal.criterion_id.clone(),
        idempotency_key: proposal.idempotency_key.clone(),
        ..Default::default()
    }
}

pub fn steer_ulw_loop_batch(
    repo_root: impl AsRef<Path>,
    proposals: &[UlwLoopSteeringProposal],
    scope: Option<&UlwLoopScope>,
) -> Result<SteerUlwLoopBatchResult, UlwLoopError> {
    with_ulw_loop_mutation_lock(&repo_root, scope, None, || {
        let plan = read_ulw_loop_plan(&repo_root, scope)?;
        let mut items = Vec::new();
        let mut results = Vec::new();
        let mut current = plan.clone();
        // The ledger cannot change before the final commit, so read it once, on first use.
        let mut ledger: Option<Vec<UlwLoopLedgerEntry>> = None;

        for proposal in proposals {
            let key = proposal
                .idempotency_key
                .as_deref()
                .or(proposal.prompt_signature.as_deref());

            let prior = match key {
                Some(k) => find_accepted_steering_entry(
                    ledger.get_or_insert_with(|| read_ledger(&repo_root, scope)),
                    k,
                )
                .cloned(),
                None => None,
            };

            if let Some(p) = prior {
                if let Some(mut st) = p.steering {
                    st.deduped = Some(true);
                    let res = SteerUlwLoopBatchItemResult {
                        accepted: true,
                        deduped: true,
                        audit: st,
                        rejected_reasons: Vec::new(),
                    };
                    items.push(PreparedItem::Deduped(res.clone()));
                    results.push(res);
                    continue;
                }
            }

            let proposal_val = serde_json::to_value(proposal).unwrap_or(Value::Null);
            let audit = validate_ulw_loop_steering_proposal(&current, &proposal_val);

            if !audit.invariant.accepted {
                let res = SteerUlwLoopBatchItemResult {
                    accepted: false,
                    deduped: false,
                    rejected_reasons: audit.invariant.rejected_reasons.clone(),
                    audit,
                };
                items.push(PreparedItem::Deduped(res.clone()));
                results.push(res);
                continue;
            }

            let next = apply_steering_mutation(&current, proposal, &audit, None);
            let changed = changed_goal_ids_between(&current, &next);
            let mut final_audit = audit.clone();
            final_audit.before = Some(build_steering_plan_snapshot(&current, &changed));
            final_audit.after = Some(build_steering_plan_snapshot(&next, &changed));

            let res = SteerUlwLoopBatchItemResult {
                accepted: true,
                deduped: false,
                audit: final_audit.clone(),
                rejected_reasons: Vec::new(),
            };

            items.push(PreparedItem::Fresh(PreparedFresh {
                proposal: proposal.clone(),
                audit: final_audit,
                next: next.clone(),
            }));
            results.push(res);
            current = next;
        }

        if let Some(failed) = results.iter().find(|r| !r.accepted) {
            let entry = rejected_ledger_entry(&results);
            let mut entries = vec![entry];
            let mut p = plan.clone();
            commit(
                &repo_root,
                scope,
                CommitMutation {
                    plan: &mut p,
                    entries: &mut entries,
                },
            )?;
            return Ok(SteerUlwLoopBatchResult {
                plan,
                accepted: false,
                rejected_reasons: failed.rejected_reasons.clone(),
                results,
            });
        }

        let mut next = plan.clone();
        for item in &items {
            if let PreparedItem::Fresh(f) = item {
                next = f.next.clone();
            }
        }

        let fresh_items: Vec<&PreparedFresh> = items
            .iter()
            .filter_map(|it| match it {
                PreparedItem::Fresh(f) => Some(f),
                _ => None,
            })
            .collect();

        if !fresh_items.is_empty() {
            let mut entries: Vec<UlwLoopLedgerEntry> = fresh_items
                .iter()
                .map(|f| item_ledger_entry(&f.proposal, &f.audit, &iso()))
                .collect();

            if let Some(batch_entry) = batch_update_ledger_entry(&plan, &next, &iso()) {
                entries.push(batch_entry);
            }

            commit(
                &repo_root,
                scope,
                CommitMutation {
                    plan: &mut next,
                    entries: &mut entries,
                },
            )?;
        }

        Ok(SteerUlwLoopBatchResult {
            plan: next,
            accepted: true,
            results,
            rejected_reasons: Vec::new(),
        })
    })
}
