//! Quality gate template generation for checkpointing.

use std::collections::BTreeMap;
use std::path::Path;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::{ulw_loop_attempt_evidence_dir, UlwLoopScope};
use crate::plan_io::read_ulw_loop_plan;
use crate::runtime::UlwLoopError;
use crate::surface::{resolve_toolkit_surface, UlwLoopToolkitSurface};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointTemplate {
    #[serde(rename = "qualityGateTemplate")]
    pub quality_gate_template: Value,
    #[serde(rename = "codexGoalTemplate")]
    pub codex_goal_template: Value,
    #[serde(rename = "attemptDir", default, skip_serializing_if = "Option::is_none")]
    pub attempt_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

pub struct CheckpointTemplateDependencies {
    pub surface: Option<UlwLoopToolkitSurface>,
}

fn artifact_path(base: &str, name: &str) -> String {
    format!("{base}/{name}")
}

fn gate_template(surface: UlwLoopToolkitSurface, base: &str) -> Value {
    let artifacts = serde_json::json!([
        {
            "id": "artifact-cli",
            "kind": "cli-transcript",
            "description": "<replace:artifact description>",
            "path": artifact_path(base, "cli-transcript.txt"),
        },
        {
            "id": "artifact-data",
            "kind": "data-diff",
            "description": "<replace:artifact description>",
            "path": artifact_path(base, "data-diff.txt"),
        }
    ]);

    let manual_qa = serde_json::json!({
        "by": "main-session",
        "status": "passed",
        "evidence": "<replace:manual QA evidence>",
        "surfaceEvidence": [
            {
                "id": "surface-cli",
                "criterionRef": "<replace:criterion id>",
                "surface": "cli",
                "invocation": "<replace:command>",
                "verdict": "passed",
                "artifactRefs": ["artifact-cli"],
            },
            {
                "id": "surface-data",
                "criterionRef": "<replace:criterion id>",
                "surface": "data",
                "invocation": "<replace:command>",
                "verdict": "passed",
                "artifactRefs": ["artifact-data"],
            }
        ],
        "adversarialCases": [
            {
                "id": "<replace:adversarial case id>",
                "criterionRef": "<replace:criterion id>",
                "scenario": "<replace:scenario>",
                "expectedBehavior": "<replace:expected behavior>",
                "verdict": "not_applicable",
                "reason": "<replace:reason>",
                "artifactRefs": ["artifact-cli"],
            }
        ],
        "artifactRefs": artifacts,
    });

    let gate_review_by = if surface == UlwLoopToolkitSurface::OmoSenpi {
        "category:deep-high"
    } else {
        "main-session"
    };

    serde_json::json!({
        "manualQa": manual_qa,
        "gateReview": {
            "by": gate_review_by,
            "recommendation": "APPROVE",
            "reportPath": artifact_path(base, "gate-review.md"),
            "evidence": "<replace:gate review evidence>",
            "blockers": [],
            "notes": [],
        },
        "iteration": {
            "fullRerun": true,
            "status": "passed",
            "rerunCommands": ["<replace:verification command>"],
            "evidence": "<replace:iteration evidence>",
        },
        "criteriaCoverage": {
            "totalCriteria": 0,
            "passCount": 0,
            "originalIntent": "<replace:original intent>",
            "desiredOutcome": "<replace:desired outcome>",
            "userOutcomeReview": "<replace:user outcome review>",
            "adversarialClassesCovered": ["<replace:adversarial class>"],
        }
    })
}

pub fn checkpoint_template(
    repo_root: impl AsRef<Path>,
    scope: Option<&UlwLoopScope>,
    goal_id: Option<&str>,
    dependencies: Option<&CheckpointTemplateDependencies>,
) -> Result<CheckpointTemplate, UlwLoopError> {
    let plan = read_ulw_loop_plan(&repo_root, scope)?;
    let surface = dependencies
        .and_then(|d| d.surface)
        .unwrap_or_else(resolve_toolkit_surface);

    let target_id = goal_id.or(plan.active_goal_id.as_deref());
    let active = target_id.and_then(|id| plan.goals.iter().find(|g| g.id == id));

    if let Some(gid) = goal_id {
        if active.is_none() {
            let mut details = BTreeMap::new();
            details.insert("goalId".into(), serde_json::json!(gid));
            return Err(UlwLoopError::with_details(
                format!("Unknown ulw-loop id: {gid}."),
                "ULW_LOOP_GOAL_NOT_FOUND",
                details,
            ));
        }
    }

    let has_attempt = plan.evidence_layout_version == Some(2) && active.is_some();
    let fallback_scope = UlwLoopScope::new("cli-test");
    let attempt_dir = if let (Some(a), true) = (active, has_attempt) {
        let template_scope = scope.or(Some(&fallback_scope));
        ulw_loop_attempt_evidence_dir(&a.id, a.attempt, template_scope)?
    } else {
        ".omo/evidence".to_string()
    };

    let mut guidance_parts = vec![
        "codex-goal-json requires goal.objective to equal the plan's codexObjective verbatim; do not paraphrase it.",
        "Fill every <replace:...> value with plausible non-empty evidence and use real, non-empty artifact files.",
        "Passing codex-goal-json example: {\"goal\":{\"objective\":\"<plan codexObjective verbatim>\",\"status\":\"complete\"}}.",
        "Passing quality-gate-json example requires gateReview {\"by\":\"category:deep-high\",\"recommendation\":\"APPROVE\",\"evidence\":\"review passed\",\"reportPath\":\"<attemptDir>/gate-review.md\",\"blockers\":[],\"notes\":[]}, manualQa.artifactRefs objects, iteration, and criteriaCoverage.",
    ];

    if surface == UlwLoopToolkitSurface::LazyCodex {
        guidance_parts.push(
            "Self-review defaults: manualQa.by and gateReview.by are main-session. Alternatives: manualQa.by accepts lazycodex-qa-executor; gateReview.by accepts lazycodex-gate-reviewer, category:deep-high, category:deep-low, category:unspecified-high, or category:unspecified-low. Optional codeReview.by accepts lazycodex-code-reviewer or main-session."
        );
    }
    if !has_attempt {
        guidance_parts.push("This plan is evidence-layout v1; artifacts go under .omo/evidence/.");
    }

    let guidance = guidance_parts.join(" ");

    Ok(CheckpointTemplate {
        quality_gate_template: gate_template(surface, &attempt_dir),
        codex_goal_template: serde_json::json!({
            "goal": {
                "objective": plan.codex_objective.as_deref().unwrap_or("<replace:codex objective>"),
                "status": "complete"
            }
        }),
        attempt_dir: if has_attempt { Some(attempt_dir) } else { None },
        guidance: Some(guidance),
    })
}
