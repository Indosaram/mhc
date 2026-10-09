//! Operation manifest for the agent toolkit SDK.

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};

pub const ULW_LOOP_OPERATIONS: [&str; 10] = [
    "help",
    "create-goals",
    "status",
    "complete-goals",
    "checkpoint",
    "steer",
    "add-goal",
    "criteria",
    "record-evidence",
    "record-review-blockers",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolkitOperationManifest {
    pub name: String,
    pub method: String,
    pub mutating: bool,
    pub description: String,
    pub args: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolkitManifest {
    pub version: u32,
    pub name: String,
    pub operations: Vec<ToolkitOperationManifest>,
}

pub fn ulw_loop_manifest() -> ToolkitManifest {
    let operations = vec![
        ToolkitOperationManifest {
            name: "help".to_string(),
            method: "help".to_string(),
            mutating: false,
            description: "Return this manifest: every operation with its method name, argument fields, and what it does.".to_string(),
            args: BTreeMap::new(),
        },
        ToolkitOperationManifest {
            name: "create-goals".to_string(),
            method: "createGoals".to_string(),
            mutating: true,
            description: "Create the session plan from a brief; one goal per bullet, each seeded with placeholder criteria until revised.".to_string(),
            args: {
                let mut m = BTreeMap::new();
                m.insert("brief".to_string(), "string".to_string());
                m.insert("codexGoalMode".to_string(), "\"aggregate\" | \"per_story\"?".to_string());
                m.insert("force".to_string(), "boolean?".to_string());
                m.insert("validationBatchesJson".to_string(), "string?".to_string());
                m
            },
        },
        ToolkitOperationManifest {
            name: "status".to_string(),
            method: "status".to_string(),
            mutating: false,
            description: "Read the plan, its summary, structured nextActions, the stable evidenceRoot, and the active goal's currentAttemptDir.".to_string(),
            args: BTreeMap::new(),
        },
        ToolkitOperationManifest {
            name: "complete-goals".to_string(),
            method: "completeGoals".to_string(),
            mutating: true,
            description: "Acquire the next eligible goal or resume the in-progress one; returns { done: true } once the aggregate is complete.".to_string(),
            args: {
                let mut m = BTreeMap::new();
                m.insert("retryFailed".to_string(), "boolean?".to_string());
                m
            },
        },
        ToolkitOperationManifest {
            name: "checkpoint".to_string(),
            method: "checkpoint".to_string(),
            mutating: true,
            description: "Close the goal as complete, failed, or blocked with evidence; the final goal also needs the quality gate. printTemplate returns that gate's template instead.".to_string(),
            args: {
                let mut m = BTreeMap::new();
                m.insert("goalId".to_string(), "string".to_string());
                m.insert("status".to_string(), "\"complete\" | \"failed\" | \"blocked\"".to_string());
                m.insert("evidence".to_string(), "string".to_string());
                m.insert("codexGoalJson".to_string(), "string?".to_string());
                m.insert("qualityGateJson".to_string(), "string?".to_string());
                m.insert("printTemplate".to_string(), "true? (with goalId? only)".to_string());
                m
            },
        },
        ToolkitOperationManifest {
            name: "steer".to_string(),
            method: "steer".to_string(),
            mutating: true,
            description: "Propose an evidence-backed plan mutation; revise_criterion replaces a criterion's scenario, expectedEvidence, or userModel.".to_string(),
            args: {
                let mut m = BTreeMap::new();
                m.insert("kind".to_string(), "\"add_subgoal\" | \"split_subgoal\" | \"reorder_pending\" | \"revise_pending_wording\" | \"revise_criterion\" | \"annotate_ledger\" | \"mark_blocked_superseded\"".to_string());
                m.insert("source".to_string(), "\"finding\" | \"user_prompt_submit\" | \"cli\"".to_string());
                m.insert("evidence".to_string(), "string".to_string());
                m.insert("rationale".to_string(), "string".to_string());
                m.insert("goalId".to_string(), "string?".to_string());
                m.insert("criterionId".to_string(), "string? (revise_criterion)".to_string());
                m.insert("scenario".to_string(), "string? (revise_criterion)".to_string());
                m.insert("expectedEvidence".to_string(), "string? (revise_criterion)".to_string());
                m.insert("userModel".to_string(), "\"happy\" | \"edge\" | \"regression\" | \"adversarial\"? (revise_criterion)".to_string());
                m.insert("title".to_string(), "string? (add_subgoal)".to_string());
                m.insert("objective".to_string(), "string? (add_subgoal)".to_string());
                m.insert("childGoals".to_string(), "{ title: string, objective: string }[]? (split_subgoal)".to_string());
                m.insert("revisedTitle".to_string(), "string? (revise_pending_wording)".to_string());
                m.insert("revisedObjective".to_string(), "string? (revise_pending_wording)".to_string());
                m.insert("pendingOrder".to_string(), "string[]? (reorder_pending)".to_string());
                m.insert("blockedReason".to_string(), "string? (mark_blocked_superseded)".to_string());
                m.insert("idempotencyKey".to_string(), "string?".to_string());
                m
            },
        },
        ToolkitOperationManifest {
            name: "add-goal".to_string(),
            method: "addGoal".to_string(),
            mutating: true,
            description: "Append a goal; pass successCriteria to define its criteria in the same call, otherwise placeholders name the revise call.".to_string(),
            args: {
                let mut m = BTreeMap::new();
                m.insert("title".to_string(), "string".to_string());
                m.insert("objective".to_string(), "string".to_string());
                m.insert("successCriteria".to_string(), "{ scenario: string, expectedEvidence: string, userModel?: \"happy\" | \"edge\" | \"regression\" | \"adversarial\", essential?: boolean }[]?".to_string());
                m
            },
        },
        ToolkitOperationManifest {
            name: "criteria".to_string(),
            method: "criteria".to_string(),
            mutating: false,
            description: "List one goal's success criteria with their status and captured evidence.".to_string(),
            args: {
                let mut m = BTreeMap::new();
                m.insert("goalId".to_string(), "string".to_string());
                m
            },
        },
        ToolkitOperationManifest {
            name: "record-evidence".to_string(),
            method: "recordEvidence".to_string(),
            mutating: true,
            description: "Record a criterion's pass, fail, or blocked evidence; artifacts must exist (resolved against the session cwd) and are stored with the criterion and the ledger entry.".to_string(),
            args: {
                let mut m = BTreeMap::new();
                m.insert("goalId".to_string(), "string".to_string());
                m.insert("criterionId".to_string(), "string".to_string());
                m.insert("status".to_string(), "\"pass\" | \"fail\" | \"blocked\"".to_string());
                m.insert("evidence".to_string(), "string".to_string());
                m.insert("notes".to_string(), "string?".to_string());
                m.insert("artifacts".to_string(), "string[]?".to_string());
                m
            },
        },
        ToolkitOperationManifest {
            name: "record-review-blockers".to_string(),
            method: "recordReviewBlockers".to_string(),
            mutating: true,
            description: "Mark the final goal review_blocked and append the blocker goal the reviewer verdict names.".to_string(),
            args: {
                let mut m = BTreeMap::new();
                m.insert("goalId".to_string(), "string".to_string());
                m.insert("title".to_string(), "string".to_string());
                m.insert("objective".to_string(), "string".to_string());
                m.insert("evidence".to_string(), "string".to_string());
                m.insert("codexGoalJson".to_string(), "string?".to_string());
                m
            },
        },
    ];

    ToolkitManifest {
        version: 1,
        name: "ulw-loop".to_string(),
        operations,
    }
}
