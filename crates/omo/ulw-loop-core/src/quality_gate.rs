//! Quality gate validation.

use std::collections::BTreeMap;
use std::path::Path;
use serde_json::Value;

use crate::domain_types::{
    UlwLoopManualQaAdversarialCase, UlwLoopManualQaArtifactRef, UlwLoopManualQaSurfaceEvidence,
    UlwLoopQualityGate, UlwLoopQualityGateCodeReview, UlwLoopQualityGateCriteriaCoverage,
    UlwLoopQualityGateGateReview, UlwLoopQualityGateIteration, UlwLoopQualityGateManualQa,
};
use crate::quality_gate_artifacts::{
    artifact_compatible, artifact_map, check_file, compatible_kinds_for, parse_artifact_refs,
    referenced_artifacts, surface_field, QualityGateCheckOptions, QualityGateFs,
};
use crate::quality_gate_fields::{
    empty_blockers, invalid, is_poisoned, is_poisoned_artifact_kind, literal_bool, literal_str,
    number_field, section, string_array, string_array_allow_empty, text_field,
    with_quality_gate_collector,
};
use crate::quality_gate_verdicts::{
    adversarial_verdict, code_quality_status_field, passed_verdict,
};
use crate::runtime::UlwLoopError;
use crate::surface::{reviewer_roles_for, UlwLoopToolkitSurface};

pub use crate::plan_crud::clear_goal_blocker_fields;
pub use crate::quality_gate_blockers::{
    classify_external_authorization_blocker, normalize_blocker_evidence,
    same_blocker_occurrences,
};

pub struct ValidateQualityGateOptions<'a> {
    pub repo_root: Option<&'a Path>,
    pub fs: Option<&'a dyn QualityGateFs>,
    pub current_attempt_dir: Option<&'a str>,
    pub reviewer_surface: Option<UlwLoopToolkitSurface>,
}

fn accepted_reviewers(surface: UlwLoopToolkitSurface, section: &str) -> &'static [&'static str] {
    match (surface, section) {
        (UlwLoopToolkitSurface::LazyCodex, "codeReview") => &["lazycodex-code-reviewer", "main-session"],
        (UlwLoopToolkitSurface::LazyCodex, "manualQa") => &["lazycodex-qa-executor", "main-session"],
        (UlwLoopToolkitSurface::LazyCodex, "gateReview") => &[
            "lazycodex-gate-reviewer",
            "category:deep-high",
            "category:deep-low",
            "category:deep",
            "category:unspecified-high",
            "category:unspecified-low",
            "main-session",
        ],
        (UlwLoopToolkitSurface::OmoSenpi, "manualQa") => &["main-session", "omo-native-qa-executor"],
        (UlwLoopToolkitSurface::OmoSenpi, "gateReview") => &[
            "category:deep-high",
            "category:deep-low",
            "category:deep",
            "category:unspecified-high",
            "category:unspecified-low",
        ],
        _ => &[],
    }
}

fn reviewer_acceptor_field(
    value: Option<&Value>,
    surface: UlwLoopToolkitSurface,
    section_name: &str,
) -> String {
    let field = format!("{section_name}.by");
    let accepted = accepted_reviewers(surface, section_name);
    let s = value.and_then(|v| v.as_str()).unwrap_or("").trim();
    if s.is_empty() {
        invalid(format!("Final quality gate requires non-empty {field}."), &field);
        return accepted.first().copied().unwrap_or("").to_string();
    }
    if !accepted.contains(&s) {
        invalid(
            format!(
                "{field} must be one of {}.",
                if !accepted.is_empty() {
                    accepted.join(", ")
                } else {
                    "the configured reviewers".to_string()
                }
            ),
            &field,
        );
    }
    s.to_string()
}

pub fn validate_quality_gate(
    input: &Value,
    opts: Option<&ValidateQualityGateOptions>,
) -> Result<UlwLoopQualityGate, UlwLoopError> {
    with_quality_gate_collector(|| validate_quality_gate_uncollected(input, opts))
}

fn validate_quality_gate_uncollected(
    input: &Value,
    opts: Option<&ValidateQualityGateOptions>,
) -> Result<UlwLoopQualityGate, UlwLoopError> {
    let surface = opts.and_then(|o| o.reviewer_surface).unwrap_or(UlwLoopToolkitSurface::LazyCodex);
    let gate_value = if let Some(qg) = input.get("qualityGate") {
        qg
    } else {
        input
    };
    let gate = section(Some(gate_value), "qualityGate").cloned().unwrap_or_default();

    for name in &["manualQa", "gateReview", "iteration", "criteriaCoverage"] {
        section(gate.get(*name), name);
    }

    if surface == UlwLoopToolkitSurface::OmoSenpi && gate.contains_key("codeReview") {
        invalid("omo-senpi gate has no codeReview lane.", "codeReview");
    }

    let manual_qa = section(gate.get("manualQa"), "manualQa").cloned().unwrap_or_default();
    let gate_review = section(gate.get("gateReview"), "gateReview").cloned().unwrap_or_default();
    let iteration = section(gate.get("iteration"), "iteration").cloned().unwrap_or_default();
    let coverage = section(gate.get("criteriaCoverage"), "criteriaCoverage").cloned().unwrap_or_default();
    let code_review = if surface == UlwLoopToolkitSurface::LazyCodex && gate.contains_key("codeReview") {
        Some(section(gate.get("codeReview"), "codeReview").cloned().unwrap_or_default())
    } else {
        None
    };

    let manual_qa_by = reviewer_acceptor_field(manual_qa.get("by"), surface, "manualQa");
    let gate_review_by = reviewer_acceptor_field(gate_review.get("by"), surface, "gateReview");
    let manual_qa_evidence = text_field(manual_qa.get("evidence"), "manualQa.evidence");
    let gate_review_evidence = text_field(gate_review.get("evidence"), "gateReview.evidence");

    let total_criteria = number_field(coverage.get("totalCriteria"), "criteriaCoverage.totalCriteria");
    let pass_count = number_field(coverage.get("passCount"), "criteriaCoverage.passCount");
    if !is_poisoned("criteriaCoverage.passCount") && pass_count < total_criteria {
        invalid(
            "criteriaCoverage.passCount must cover totalCriteria.",
            "criteriaCoverage.passCount",
        );
    }

    let check_opts = opts.map(|o| QualityGateCheckOptions {
        repo_root: o.repo_root,
        fs: o.fs,
        current_attempt_dir: o.current_attempt_dir,
    });

    let artifact_refs = parse_artifact_refs(manual_qa.get("artifactRefs"), check_opts.as_ref());
    let by_id = artifact_map(&artifact_refs);
    let surface_evidence = parse_surface_evidence(manual_qa.get("surfaceEvidence"), &by_id);
    let adversarial_cases = parse_adversarial_cases(manual_qa.get("adversarialCases"), &by_id);

    let gate_report_path = text_field(gate_review.get("reportPath"), "gateReview.reportPath");
    if !is_poisoned("gateReview.reportPath") {
        check_file(&gate_report_path, "gateReview.reportPath", check_opts.as_ref());
    }

    let parsed_code_review = if let Some(cr) = code_review {
        let code_report_path = text_field(cr.get("reportPath"), "codeReview.reportPath");
        check_file(&code_report_path, "codeReview.reportPath", check_opts.as_ref());
        Some(UlwLoopQualityGateCodeReview {
            by: reviewer_acceptor_field(cr.get("by"), surface, "codeReview"),
            recommendation: literal_str(cr.get("recommendation"), "APPROVE", "codeReview.recommendation"),
            code_quality_status: code_quality_status_field(cr.get("codeQualityStatus"), "codeReview.codeQualityStatus"),
            report_path: code_report_path,
            evidence: text_field(cr.get("evidence"), "codeReview.evidence"),
            blockers: empty_blockers(cr.get("blockers"), "codeReview.blockers"),
        })
    } else {
        None
    };

    Ok(UlwLoopQualityGate {
        surface: surface.as_str().to_string(),
        manual_qa: UlwLoopQualityGateManualQa {
            by: manual_qa_by,
            status: passed_verdict(manual_qa.get("status"), "manualQa.status"),
            evidence: manual_qa_evidence,
            surface_evidence,
            adversarial_cases,
            artifact_refs,
        },
        gate_review: UlwLoopQualityGateGateReview {
            by: gate_review_by,
            recommendation: literal_str(gate_review.get("recommendation"), "APPROVE", "gateReview.recommendation"),
            report_path: gate_report_path,
            evidence: gate_review_evidence,
            blockers: empty_blockers(gate_review.get("blockers"), "gateReview.blockers"),
        },
        code_review: parsed_code_review,
        iteration: UlwLoopQualityGateIteration {
            full_rerun: literal_bool(iteration.get("fullRerun"), true, "iteration.fullRerun"),
            status: literal_str(iteration.get("status"), "passed", "iteration.status"),
            rerun_commands: string_array(iteration.get("rerunCommands"), "iteration.rerunCommands"),
            evidence: text_field(iteration.get("evidence"), "iteration.evidence"),
        },
        criteria_coverage: UlwLoopQualityGateCriteriaCoverage {
            total_criteria,
            pass_count,
            original_intent: text_field(coverage.get("originalIntent"), "criteriaCoverage.originalIntent"),
            desired_outcome: text_field(coverage.get("desiredOutcome"), "criteriaCoverage.desiredOutcome"),
            user_outcome_review: text_field(coverage.get("userOutcomeReview"), "criteriaCoverage.userOutcomeReview"),
            adversarial_classes_covered: string_array_allow_empty(
                coverage.get("adversarialClassesCovered"),
                "criteriaCoverage.adversarialClassesCovered",
            ),
        },
    })
}

fn parse_surface_evidence(
    value: Option<&Value>,
    by_id: &BTreeMap<String, UlwLoopManualQaArtifactRef>,
) -> Vec<UlwLoopManualQaSurfaceEvidence> {
    let Some(v) = value else {
        invalid("manualQa.surfaceEvidence must not be empty.", "manualQa.surfaceEvidence");
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        invalid("manualQa.surfaceEvidence must not be empty.", "manualQa.surfaceEvidence");
        return Vec::new();
    };
    if arr.is_empty() {
        invalid("manualQa.surfaceEvidence must not be empty.", "manualQa.surfaceEvidence");
        return Vec::new();
    }

    let mut out = Vec::new();
    for (index, item) in arr.iter().enumerate() {
        let prefix = format!("manualQa.surfaceEvidence[{index}]");
        if !item.is_object() {
            invalid(
                format!("Final quality gate requires object at {prefix}."),
                &prefix,
            );
            continue;
        }
        let Some(row) = section(Some(item), &prefix) else { continue };
        if is_poisoned(&prefix) {
            continue;
        }

        let surf_field = format!("{prefix}.surface");
        let surface = surface_field(row.get("surface"), &surf_field);
        let art_field = format!("{prefix}.artifactRefs");
        let artifacts = referenced_artifacts(row.get("artifactRefs"), &art_field, by_id);

        for artifact in &artifacts {
            if is_poisoned(&surf_field) || is_poisoned_artifact_kind(&artifact.id) {
                continue;
            }
            if !artifact_compatible(&surface, &artifact.kind) {
                invalid(
                    format!(
                        "manualQa.surfaceEvidence {surface} artifact {} is incompatible; surface \"{surface}\" accepts artifact kinds: {}.",
                        artifact.kind,
                        compatible_kinds_for(&surface).join(", ")
                    ),
                    "manualQa.surfaceEvidence",
                );
            }
        }

        out.push(UlwLoopManualQaSurfaceEvidence {
            id: text_field(row.get("id"), &format!("{prefix}.id")),
            criterion_ref: text_field(row.get("criterionRef"), &format!("{prefix}.criterionRef")),
            surface,
            invocation: text_field(row.get("invocation"), &format!("{prefix}.invocation")),
            verdict: passed_verdict(row.get("verdict"), &format!("{prefix}.verdict")),
            artifact_refs: artifacts.into_iter().map(|a| a.id).collect(),
        });
    }
    out
}

fn parse_adversarial_cases(
    value: Option<&Value>,
    by_id: &BTreeMap<String, UlwLoopManualQaArtifactRef>,
) -> Vec<UlwLoopManualQaAdversarialCase> {
    let Some(v) = value else {
        invalid("manualQa.adversarialCases must not be empty.", "manualQa.adversarialCases");
        return Vec::new();
    };
    let Some(arr) = v.as_array() else {
        invalid("manualQa.adversarialCases must not be empty.", "manualQa.adversarialCases");
        return Vec::new();
    };
    if arr.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for (index, item) in arr.iter().enumerate() {
        let prefix = format!("manualQa.adversarialCases[{index}]");
        if !item.is_object() {
            invalid(
                format!("Final quality gate requires object at {prefix}."),
                &prefix,
            );
            continue;
        }
        let Some(row) = section(Some(item), &prefix) else { continue };
        if is_poisoned(&prefix) {
            continue;
        }

        let art_field = format!("{prefix}.artifactRefs");
        let artifacts = referenced_artifacts(row.get("artifactRefs"), &art_field, by_id);
        let (verdict, reason) = adversarial_verdict(row, &prefix);

        out.push(UlwLoopManualQaAdversarialCase {
            id: text_field(row.get("id"), &format!("{prefix}.id")),
            criterion_ref: text_field(row.get("criterionRef"), &format!("{prefix}.criterionRef")),
            scenario: text_field(row.get("scenario"), &format!("{prefix}.scenario")),
            expected_behavior: text_field(row.get("expectedBehavior"), &format!("{prefix}.expectedBehavior")),
            verdict,
            reason,
            artifact_refs: artifacts.into_iter().map(|a| a.id).collect(),
        });
    }
    out
}
