//! Steering validation and mutation engine.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;
use regex::Regex;
use serde_json::Value;

use crate::domain_types::{UlwLoopItem, UlwLoopLedgerEntry, UlwLoopPlan};
use crate::goal_status::is_ulw_loop_done;
use crate::paths::UlwLoopScope;
use crate::plan_commit::{commit, CommitMutation};
use crate::plan_io::{find_accepted_steering_ledger_entry, read_ulw_loop_plan, with_ulw_loop_mutation_lock};
use crate::runtime::{iso, UlwLoopError};
use crate::steering_mutations::{make_goal, revise_criterion, revise_wording, split_or_block};
use crate::steering_snapshot::{build_steering_plan_snapshot, changed_goal_ids_between};
use crate::steering_types::{
    SteerUlwLoopResult, UlwLoopSteeringAudit, UlwLoopSteeringChildGoal, UlwLoopSteeringInvariantAudit,
    UlwLoopSteeringMutationKind, UlwLoopSteeringProposal, UlwLoopSteeringSource,
};
use crate::surface::UlwLoopToolkitSurface;
use crate::validation_batch::batch_update_ledger_entry;

const SOURCES: [&str; 3] = ["user_prompt_submit", "finding", "cli"];
const KINDS: [&str; 7] = [
    "add_subgoal",
    "split_subgoal",
    "reorder_pending",
    "revise_pending_wording",
    "mark_blocked_superseded",
    "revise_criterion",
    "annotate_ledger",
];
const USER_MODELS: [&str; 3] = ["novice_external", "intermediate_team", "expert_adversarial"];
const PROTECTED: [&str; 9] = [
    "aggregateCompletion",
    "codexObjective",
    "codexObjectiveAliases",
    "acknowledgedDriverObjectives",
    "originalConstraints",
    "qualityGate",
    "status",
    "completedAt",
    "completionStatus",
];

fn is_valid_kind(s: &str) -> bool {
    KINDS.contains(&s)
}

fn is_valid_source(s: &str) -> bool {
    SOURCES.contains(&s)
}

fn is_valid_user_model(s: &str) -> bool {
    USER_MODELS.contains(&s)
}

fn get_field<'a>(val: &'a Value, key: &str) -> Option<&'a Value> {
    val.as_object().and_then(|obj| obj.get(key))
}

fn get_text(val: &Value, key: &str) -> Option<String> {
    get_field(val, key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn get_texts(val: &Value, key: &str) -> Vec<String> {
    get_field(val, key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn after_obj<'a>(val: &'a Value) -> Option<&'a Value> {
    get_field(val, "after").filter(|v| v.is_object())
}

fn revised_str(val: &Value, direct: &str, nested: &str) -> Option<String> {
    get_text(val, direct).or_else(|| {
        let nested_obj = after_obj(val).unwrap_or(val);
        get_text(nested_obj, nested)
    })
}

fn extract_targets(proposal: &Value) -> Vec<String> {
    let many = get_texts(proposal, "targetGoalIds");
    if !many.is_empty() {
        return many;
    }
    let one = get_text(proposal, "targetGoalId").or_else(|| get_text(proposal, "goalId"));
    one.into_iter().collect()
}

fn child_val(val: &Value) -> Option<UlwLoopSteeringChildGoal> {
    if !val.is_object() {
        return None;
    }
    let title = get_text(val, "title")?;
    let objective = get_text(val, "objective")?;
    Some(UlwLoopSteeringChildGoal { title, objective })
}

fn child_values(proposal: &Value) -> Vec<Value> {
    if let Some(arr) = get_field(proposal, "childGoals").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            return arr.clone();
        }
    }
    if let Some(nested) = after_obj(proposal) {
        if let Some(from_after) = get_field(nested, "children").and_then(|v| v.as_array()) {
            return from_after.clone();
        }
    }
    Vec::new()
}

fn pending_order(proposal: &Value) -> Vec<String> {
    let direct = get_texts(proposal, "pendingOrder");
    if !direct.is_empty() {
        return direct;
    }
    let nested = after_obj(proposal).unwrap_or(proposal);
    get_texts(nested, "pendingGoalIds")
}

fn has_protected(val: &Value) -> bool {
    if let Some(obj) = val.as_object() {
        for (k, v) in obj {
            if PROTECTED.contains(&k.as_str()) || k.to_lowercase().contains("complete") || has_protected(v) {
                return true;
            }
        }
    } else if let Some(arr) = val.as_array() {
        for v in arr {
            if has_protected(v) {
                return true;
            }
        }
    }
    false
}

fn all_text(val: &Value) -> String {
    if let Some(s) = val.as_str() {
        return s.to_string();
    }
    if let Some(obj) = val.as_object() {
        return obj.values().map(all_text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
    }
    if let Some(arr) = val.as_array() {
        return arr.iter().map(all_text).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
    }
    String::new()
}

fn weakens(val: &Value) -> bool {
    let text = all_text(val).to_lowercase();
    // PERF-DIVERGENCE(omo steering): upstream builds these RegExps on every call; this port compiles them once in LazyLocks. Output is identical.
    static RE_WEAKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(skip|bypass|weaken|remove|omit|auto[-\s]?complete|mark complete|complete faster)\b").expect("valid regex"));
    static RE_TARGET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(test|tests|verification|review|quality gate|complete|completion)\b").expect("valid regex"));
    RE_WEAKEN.is_match(&text) && RE_TARGET.is_match(&text)
}

fn audit_for(proposal: &Value, reasons: Vec<String>) -> UlwLoopSteeringAudit {
    let kind_raw = get_field(proposal, "kind").and_then(|v| v.as_str());
    let source_raw = get_field(proposal, "source").and_then(|v| v.as_str());
    let evidence = get_text(proposal, "evidence").unwrap_or_default();
    let rationale = get_text(proposal, "rationale").unwrap_or_default();

    let kind = if let Some(k) = kind_raw {
        if is_valid_kind(k) {
            k.to_string()
        } else {
            "annotate_ledger".to_string()
        }
    } else {
        "annotate_ledger".to_string()
    };

    let source = if let Some(s) = source_raw {
        if is_valid_source(s) {
            s.to_string()
        } else {
            "cli".to_string()
        }
    } else {
        "cli".to_string()
    };

    let accepted = reasons.is_empty();
    let audit = UlwLoopSteeringAudit {
        kind,
        source,
        target_goal_ids: extract_targets(proposal),
        evidence: evidence.clone(),
        rationale: rationale.clone(),
        criterion_id: get_text(proposal, "criterionId"),
        directive_text: get_text(proposal, "directiveText"),
        prompt_signature: get_text(proposal, "promptSignature"),
        idempotency_key: get_text(proposal, "idempotencyKey"),
        invariant: UlwLoopSteeringInvariantAudit {
            accepted,
            structural_invariant_accepted: accepted,
            evidence_backed_necessity: !evidence.is_empty() && !rationale.is_empty(),
            no_easier_completion: !weakens(proposal),
            rejected_reasons: reasons.clone(),
            reasons: Some(reasons),
        },
        deduped: None,
        before: None,
        after: None,
    };

    audit
}

pub fn validate_ulw_loop_steering_proposal(plan: &UlwLoopPlan, proposal: &Value) -> UlwLoopSteeringAudit {
    let mut reasons = Vec::new();
    if !proposal.is_object() {
        reasons.push("proposal must be an object".to_string());
    }

    let kind_val = get_field(proposal, "kind");
    let kind_str = kind_val.and_then(|v| v.as_str());
    if kind_str.map(is_valid_kind) != Some(true) {
        reasons.push(format!("invalid kind: {}", kind_val.unwrap_or(&Value::Null)));
    }

    let src_val = get_field(proposal, "source");
    let src_str = src_val.and_then(|v| v.as_str());
    if src_str.map(is_valid_source) != Some(true) {
        reasons.push(format!("invalid source: {}", src_val.unwrap_or(&Value::Null)));
    }

    if get_text(proposal, "evidence").is_none() {
        reasons.push("missing evidence".to_string());
    }
    if get_text(proposal, "rationale").is_none() {
        reasons.push("missing rationale".to_string());
    }
    if has_protected(proposal) {
        reasons.push("protected payload".to_string());
    }
    if weakens(proposal) {
        reasons.push("weakened completion".to_string());
    }
    if is_ulw_loop_done(plan) {
        reasons.push("plan already complete".to_string());
    }

    if let Some(k) = kind_str {
        if is_valid_kind(k) {
            validate_kind(plan, proposal, k, &mut reasons);
        }
    }

    audit_for(proposal, reasons)
}

fn validate_kind(plan: &UlwLoopPlan, proposal: &Value, kind: &str, reasons: &mut Vec<String>) {
    let target_ids = extract_targets(proposal);
    let target = target_ids.first().and_then(|id| plan.goals.iter().find(|g| g.id == *id));

    if kind == "add_subgoal" && (get_text(proposal, "title").is_none() || get_text(proposal, "objective").is_none()) {
        reasons.push("add_subgoal requires title/objective".to_string());
    }

    if (kind == "split_subgoal" || kind == "revise_pending_wording" || kind == "mark_blocked_superseded") && target.is_none() {
        reasons.push(format!("{kind} requires target"));
    }

    if (kind == "split_subgoal" || kind == "revise_pending_wording") && target.is_some_and(|t| t.status != "pending") {
        reasons.push(format!("{kind} requires pending target"));
    }

    let raw_children = child_values(proposal);
    if kind == "split_subgoal" && raw_children.is_empty() {
        reasons.push("split_subgoal requires children".to_string());
    }
    if (kind == "split_subgoal" || kind == "mark_blocked_superseded") && raw_children.iter().any(|c| child_val(c).is_none()) {
        reasons.push(format!("{kind} children require title/objective"));
    }

    if kind == "reorder_pending" {
        validate_order(plan, proposal, reasons);
    }
    if kind == "revise_pending_wording"
        && revised_str(proposal, "revisedTitle", "title").is_none()
        && revised_str(proposal, "revisedObjective", "objective").is_none()
    {
        reasons.push("revise_pending_wording requires update".to_string());
    }
    if kind == "revise_criterion" {
        validate_criterion(plan, proposal, reasons);
    }
}

fn validate_order(plan: &UlwLoopPlan, proposal: &Value, reasons: &mut Vec<String>) {
    let requested = pending_order(proposal);
    let pending: Vec<String> = plan
        .goals
        .iter()
        .filter(|g| g.status == "pending" && g.steering_status.is_none())
        .map(|g| g.id.clone())
        .collect();

    if requested.is_empty() {
        reasons.push("reorder_pending requires ids".to_string());
    }
    let mut set = BTreeSet::new();
    for id in &requested {
        if !set.insert(id) {
            reasons.push("duplicate pending id".to_string());
            break;
        }
    }
    if requested.iter().any(|id| !pending.contains(id)) {
        reasons.push("unknown pending id".to_string());
    }
}

fn validate_criterion(plan: &UlwLoopPlan, proposal: &Value, reasons: &mut Vec<String>) {
    let target_ids = extract_targets(proposal);
    let target = target_ids.first().and_then(|id| plan.goals.iter().find(|g| g.id == *id));
    let criterion_id = get_text(proposal, "criterionId");

    match target {
        None => {
            reasons.push("revise_criterion requires goalId".to_string());
        }
        Some(t) => {
            if criterion_id.is_none()
                || t.success_criteria.iter().all(|c| Some(&c.id) != criterion_id.as_ref())
            {
                reasons.push("revise_criterion requires criterionId".to_string());
            }
        }
    }

    let model = get_field(proposal, "userModel");
    if get_field(proposal, "scenario").is_none()
        && get_field(proposal, "expectedEvidence").is_none()
        && model.is_none()
    {
        reasons.push("revise_criterion requires update".to_string());
    }
    if let Some(m) = model.and_then(|v| v.as_str()) {
        if !is_valid_user_model(m) {
            reasons.push("invalid userModel".to_string());
        }
    }
}

pub fn apply_steering_mutation(
    plan: &UlwLoopPlan,
    proposal: &UlwLoopSteeringProposal,
    audit: &UlwLoopSteeringAudit,
    surface: Option<UlwLoopToolkitSurface>,
) -> UlwLoopPlan {
    let mut next = plan.clone();
    if !audit.invariant.accepted {
        return next;
    }

    let now = iso();
    if proposal.kind == "add_subgoal" {
        let child_goal = UlwLoopSteeringChildGoal {
            title: proposal.title.clone().unwrap_or_default(),
            objective: proposal.objective.clone().unwrap_or_default(),
        };
        let new_g = make_goal(&next, &child_goal, &proposal.evidence, &now, 1, surface);
        next.goals.push(new_g);
    }
    if proposal.kind == "reorder_pending" {
        let order = proposal
            .pending_order
            .clone()
            .or_else(|| proposal.raw_proposal.as_ref().map(pending_order))
            .unwrap_or_default();
        let mut ordered_goals = Vec::new();
        for id in &order {
            if let Some(g) = next.goals.iter().find(|cand| cand.id == *id) {
                ordered_goals.push(g.clone());
            }
        }
        for g in &next.goals {
            if !order.contains(&g.id) {
                ordered_goals.push(g.clone());
            }
        }
        next.goals = ordered_goals;
    }
    if proposal.kind == "revise_pending_wording" {
        revise_wording(&mut next, proposal, &now);
    }
    if proposal.kind == "split_subgoal" || proposal.kind == "mark_blocked_superseded" {
        split_or_block(&mut next, proposal, &now, surface);
    }
    if proposal.kind == "revise_criterion" {
        revise_criterion(&mut next, proposal, &now);
    }
    if proposal.kind != "annotate_ledger" {
        next.updated_at = now;
    }

    next
}

pub fn parse_ulw_loop_steering_directive(text: &str) -> Option<UlwLoopSteeringProposal> {
    let re = Regex::new(r"(?:^|\s)(?:OMO_ULW_LOOP_STEER|omo\.ulw-loop\.steer|omo ulw-loop steer|ulw-loop steer):\s*([\s\S]+)$").expect("valid regex");
    let captures = re.captures(text)?;
    let raw = captures.get(1)?.as_str().trim();
    let val: Value = serde_json::from_str(raw).ok()?;
    if !val.is_object() {
        return None;
    }
    let kind = get_text(&val, "kind")?;
    let source = get_text(&val, "source")?;
    let evidence = get_text(&val, "evidence")?;
    let rationale = get_text(&val, "rationale")?;

    if !is_valid_kind(&kind) || !is_valid_source(&source) {
        return None;
    }

    let mut proposal: UlwLoopSteeringProposal = serde_json::from_value(val.clone()).ok()?;
    proposal.raw_proposal = Some(val);
    Some(proposal)
}

fn build_ledger_entry(
    proposal: &UlwLoopSteeringProposal,
    audit: &UlwLoopSteeringAudit,
    at: &str,
) -> UlwLoopLedgerEntry {
    let kind = if audit.invariant.accepted {
        if proposal.kind == "revise_criterion" {
            "criteria_revised".to_string()
        } else {
            "steering_accepted".to_string()
        }
    } else {
        "steering_rejected".to_string()
    };

    let goal_id = audit.target_goal_ids.first().cloned();
    UlwLoopLedgerEntry {
        at: at.to_string(),
        kind,
        goal_id,
        evidence: Some(proposal.evidence.clone()),
        message: Some(proposal.rationale.clone()),
        steering: Some(audit.clone()),
        mutation_kind: Some(proposal.kind.clone()),
        criterion_id: proposal.criterion_id.clone(),
        idempotency_key: proposal.idempotency_key.clone(),
        ..Default::default()
    }
}

pub fn steer_ulw_loop(
    repo_root: impl AsRef<Path>,
    proposal: &UlwLoopSteeringProposal,
    scope: Option<&UlwLoopScope>,
    surface: Option<UlwLoopToolkitSurface>,
) -> Result<SteerUlwLoopResult, UlwLoopError> {
    with_ulw_loop_mutation_lock(&repo_root, scope, None, || {
        let plan = read_ulw_loop_plan(&repo_root, scope)?;
        let key = proposal
            .idempotency_key
            .as_deref()
            .or(proposal.prompt_signature.as_deref());

        if let Some(k) = key {
            if let Some(prior) = find_accepted_steering_ledger_entry(&repo_root, k, scope) {
                if let Some(st) = prior.steering {
                    let mut compact_audit = st;
                    compact_audit.before = None;
                    compact_audit.after = None;
                    compact_audit.deduped = Some(true);
                    return Ok(SteerUlwLoopResult {
                        plan,
                        accepted: true,
                        audit: compact_audit,
                        rejected_reasons: Vec::new(),
                        deduped: true,
                    });
                }
            }
        }

        let proposal_val = serde_json::to_value(proposal).unwrap_or(Value::Null);
        let audit = validate_ulw_loop_steering_proposal(&plan, &proposal_val);
        let accepted = audit.invariant.accepted;
        let mut next = if accepted {
            apply_steering_mutation(&plan, proposal, &audit, surface)
        } else {
            plan.clone()
        };

        let mut final_audit = audit.clone();
        if accepted {
            let changed = changed_goal_ids_between(&plan, &next);
            final_audit.before = Some(build_steering_plan_snapshot(&plan, &changed));
            final_audit.after = Some(build_steering_plan_snapshot(&next, &changed));
        }

        let at = iso();
        let batch_entry = if accepted {
            batch_update_ledger_entry(&plan, &next, &at)
        } else {
            None
        };

        let mut entries = vec![build_ledger_entry(proposal, &final_audit, &at)];
        if let Some(be) = batch_entry {
            entries.push(be);
        }

        commit(
            &repo_root,
            scope,
            CommitMutation {
                plan: &mut next,
                entries: &mut entries,
            },
        )?;

        Ok(SteerUlwLoopResult {
            plan: next,
            accepted,
            rejected_reasons: audit.invariant.rejected_reasons,
            audit: final_audit,
            deduped: false,
        })
    })
}
