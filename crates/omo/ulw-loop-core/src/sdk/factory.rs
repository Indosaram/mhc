//! SDK implementation and factory.

use std::collections::BTreeMap;
use std::path::Path;
use serde_json::Value;

use crate::checkpoint::{checkpoint_ulw_loop, CheckpointUlwLoopArgs, CheckpointUlwLoopDependencies, CheckpointUlwLoopResult};
use crate::checkpoint_template::{checkpoint_template, CheckpointTemplate, CheckpointTemplateDependencies};
use crate::command_types::{AddGoalOptions, CreateGoalsOptions, RecordEvidenceOptions, RecordFinalReviewBlockersOptions};
use crate::domain_types::{UlwLoopPlan, UlwLoopSuccessCriterion};
use crate::evidence::{record_evidence, RecordEvidenceArgs, RecordEvidenceResult};
use crate::paths::{
    normalize_ulw_loop_session_id, ulw_loop_attempt_evidence_dir, ulw_loop_evidence_root,
    ulw_loop_goals_relative_path, UlwLoopScope,
};
use crate::plan_crud::{
    add_ulw_loop_goal, create_ulw_loop_plan, start_next_ulw_loop, summarize_ulw_loop_plan,
    AddGoalResult, StartNextResult,
};
use crate::plan_io::{list_ulw_loop_session_ids, read_ulw_loop_plan, read_ulw_loop_plan_sync};
use crate::plan_missing_recovery::plan_missing_error;
use crate::review_blockers::{record_final_review_blockers, RecordFinalReviewBlockersResult};
use crate::runtime::UlwLoopError;
use crate::sdk::manifest::{ulw_loop_manifest, ToolkitManifest, ULW_LOOP_OPERATIONS};
use crate::sdk::types::{
    CheckpointArgs, CompleteGoalsArgs, CriteriaResult, StatusResult, ToolkitContext,
    ToolkitDispatchRequest, ToolkitError, ToolkitResponse,
};
use crate::status_next_actions::status_next_actions;
use crate::steering::steer_ulw_loop;
use crate::steering_types::{SteerUlwLoopResult, UlwLoopSteeringProposal};
use crate::surface::UlwLoopToolkitSurface;

pub trait ToolkitHooks: Send + Sync {
    fn on_operation(&self, operation: &str, context: &ToolkitContext, response: &Value);
}

pub struct AgentToolkitDependencies {
    pub hooks: Option<Box<dyn ToolkitHooks>>,
}

pub struct AgentToolkit {
    pub context: ToolkitContext,
    pub deps: Option<AgentToolkitDependencies>,
}

fn validate_context(context: &ToolkitContext) -> Result<(), UlwLoopError> {
    if context.cwd.trim().is_empty() {
        return Err(UlwLoopError::new("cwd is required.", "ULW_LOOP_CWD_REQUIRED"));
    }
    let session_id = context.session_id.trim();
    if session_id.is_empty() {
        return Err(UlwLoopError::new(
            "ULW_LOOP_SESSION_ID_REQUIRED: sessionId is required.",
            "ULW_LOOP_SESSION_ID_REQUIRED",
        ));
    }
    let normalized = normalize_ulw_loop_session_id(Some(session_id));
    if normalized.is_none() || session_id.contains("../") || session_id.contains("..\\") {
        return Err(UlwLoopError::new(
            "ULW_LOOP_SESSION_ID_INVALID: sessionId normalizes to null.",
            "ULW_LOOP_SESSION_ID_INVALID",
        ));
    }
    Ok(())
}

fn error_to_toolkit_error(err: &UlwLoopError) -> ToolkitError {
    let details = err.details.as_ref().map(|d| {
        d.iter()
            .map(|(k, v)| (k.clone(), v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())))
            .collect()
    });
    ToolkitError {
        code: err.code.clone(),
        message: err.message.clone(),
        details,
    }
}

impl AgentToolkit {
    pub fn new(context: ToolkitContext, deps: Option<AgentToolkitDependencies>) -> Result<Self, UlwLoopError> {
        validate_context(&context)?;
        Ok(Self { context, deps })
    }

    fn notify<T: serde::Serialize>(&self, op: &str, resp: &ToolkitResponse<T>) {
        if let Some(d) = &self.deps {
            if let Some(h) = &d.hooks {
                let val = serde_json::to_value(resp).unwrap_or(Value::Null);
                h.on_operation(op, &self.context, &val);
            }
        }
    }

    fn scope(&self) -> UlwLoopScope {
        UlwLoopScope {
            session_id: Some(self.context.session_id.clone()),
            ..Default::default()
        }
    }

    pub fn help(&self) -> ToolkitResponse<ToolkitManifest> {
        let manifest = ulw_loop_manifest();
        let resp = ToolkitResponse::Success {
            ok: true,
            operation: "help".to_string(),
            result: manifest,
            next_actions: Vec::new(),
            warnings: Vec::new(),
        };
        self.notify("help", &resp);
        resp
    }

    pub fn create_goals(&self, args: &CreateGoalsOptions) -> ToolkitResponse<UlwLoopPlan> {
        let scope = self.scope();
        let res = create_ulw_loop_plan(&self.context.cwd, args, Some(&scope), Some(self.context.surface));
        match res {
            Ok(plan) => {
                let next_actions = status_next_actions(&plan, Some(self.context.surface));
                let resp = ToolkitResponse::Success {
                    ok: true,
                    operation: "create-goals".to_string(),
                    result: plan,
                    next_actions,
                    warnings: Vec::new(),
                };
                self.notify("create-goals", &resp);
                resp
            }
            Err(err) => {
                let resp = ToolkitResponse::Failure {
                    ok: false,
                    operation: "create-goals".to_string(),
                    error: error_to_toolkit_error(&err),
                    warnings: Vec::new(),
                };
                self.notify("create-goals", &resp);
                resp
            }
        }
    }

    pub fn status(&self) -> ToolkitResponse<StatusResult> {
        let scope = self.scope();
        let plan_res = read_ulw_loop_plan_sync(&self.context.cwd, Some(&scope));
        match plan_res {
            Ok(plan) => {
                let summary = summarize_ulw_loop_plan(&plan);
                let next_actions = status_next_actions(&plan, Some(self.context.surface));
                let evidence_root = ulw_loop_evidence_root(Some(&scope));
                let active = plan.active_goal_id.as_deref().and_then(|id| plan.goals.iter().find(|g| g.id == id));
                let current_attempt_dir = if plan.evidence_layout_version == Some(2) {
                    active.and_then(|a| ulw_loop_attempt_evidence_dir(&a.id, a.attempt, Some(&scope)).ok())
                } else {
                    None
                };

                let result = StatusResult {
                    plan,
                    summary,
                    next_actions: next_actions.clone(),
                    evidence_root,
                    current_attempt_dir,
                };
                let resp = ToolkitResponse::Success {
                    ok: true,
                    operation: "status".to_string(),
                    result,
                    next_actions,
                    warnings: Vec::new(),
                };
                self.notify("status", &resp);
                resp
            }
            Err(err) => {
                let err_out = if err.code == "ULW_LOOP_PLAN_MISSING" && self.context.surface == UlwLoopToolkitSurface::OmoSenpi {
                    let rel = ulw_loop_goals_relative_path(Some(&scope));
                    let sids = list_ulw_loop_session_ids(&self.context.cwd);
                    plan_missing_error(&rel, &sids, Some(self.context.surface))
                } else {
                    err
                };
                let resp = ToolkitResponse::Failure {
                    ok: false,
                    operation: "status".to_string(),
                    error: error_to_toolkit_error(&err_out),
                    warnings: Vec::new(),
                };
                self.notify("status", &resp);
                resp
            }
        }
    }

    pub fn complete_goals(&self, args: Option<&CompleteGoalsArgs>) -> ToolkitResponse<StartNextResult> {
        let scope = self.scope();
        let retry_failed = args.map(|a| a.retry_failed).unwrap_or(false);
        let res = start_next_ulw_loop(&self.context.cwd, Some(&crate::command_types::StartNextOptions { retry_failed: Some(retry_failed) }), Some(&scope));
        match res {
            Ok(result) => {
                let resp = ToolkitResponse::Success {
                    ok: true,
                    operation: "complete-goals".to_string(),
                    result,
                    next_actions: Vec::new(),
                    warnings: Vec::new(),
                };
                self.notify("complete-goals", &resp);
                resp
            }
            Err(err) => {
                let resp = ToolkitResponse::Failure {
                    ok: false,
                    operation: "complete-goals".to_string(),
                    error: error_to_toolkit_error(&err),
                    warnings: Vec::new(),
                };
                self.notify("complete-goals", &resp);
                resp
            }
        }
    }

    pub fn checkpoint(&self, args: &CheckpointArgs) -> ToolkitResponse<Value> {
        let scope = self.scope();
        if args.print_template == Some(true) {
            let deps = CheckpointTemplateDependencies {
                surface: Some(self.context.surface),
            };
            match checkpoint_template(&self.context.cwd, Some(&scope), args.goal_id.as_deref(), Some(&deps)) {
                Ok(tmpl) => {
                    let val = serde_json::to_value(&tmpl).unwrap_or(Value::Null);
                    let resp = ToolkitResponse::Success {
                        ok: true,
                        operation: "checkpoint".to_string(),
                        result: val,
                        next_actions: Vec::new(),
                        warnings: Vec::new(),
                    };
                    self.notify("checkpoint", &resp);
                    resp
                }
                Err(err) => {
                    let resp = ToolkitResponse::Failure {
                        ok: false,
                        operation: "checkpoint".to_string(),
                        error: error_to_toolkit_error(&err),
                        warnings: Vec::new(),
                    };
                    self.notify("checkpoint", &resp);
                    resp
                }
            }
        } else {
            if let Some(raw) = &args.codex_goal_json {
                if serde_json::from_str::<Value>(raw).is_err() {
                    let err = UlwLoopError::new(
                        "Invalid codexGoal: not valid JSON",
                        "ULW_LOOP_CODEX_GOAL_JSON_INVALID",
                    );
                    let resp = ToolkitResponse::Failure {
                        ok: false,
                        operation: "checkpoint".to_string(),
                        error: error_to_toolkit_error(&err),
                        warnings: Vec::new(),
                    };
                    self.notify("checkpoint", &resp);
                    return resp;
                }
            }

            let cp_args = CheckpointUlwLoopArgs {
                goal_id: args.goal_id.clone().unwrap_or_default(),
                status: args.status.clone().unwrap_or_default(),
                evidence: args.evidence.clone().unwrap_or_default(),
                codex_goal_json: args.codex_goal_json.clone(),
                quality_gate_json: args.quality_gate_json.clone(),
            };
            let deps = CheckpointUlwLoopDependencies {
                surface: Some(self.context.surface),
            };

            match checkpoint_ulw_loop(&self.context.cwd, &cp_args, Some(&scope), Some(&deps)) {
                Ok(res) => {
                    let next_actions = res.next_actions.clone();
                    let warnings = res.warnings.clone();
                    let val = serde_json::to_value(&res).unwrap_or(Value::Null);
                    let resp = ToolkitResponse::Success {
                        ok: true,
                        operation: "checkpoint".to_string(),
                        result: val,
                        next_actions,
                        warnings,
                    };
                    self.notify("checkpoint", &resp);
                    resp
                }
                Err(err) => {
                    let resp = ToolkitResponse::Failure {
                        ok: false,
                        operation: "checkpoint".to_string(),
                        error: error_to_toolkit_error(&err),
                        warnings: Vec::new(),
                    };
                    self.notify("checkpoint", &resp);
                    resp
                }
            }
        }
    }

    pub fn steer(&self, args: &UlwLoopSteeringProposal) -> ToolkitResponse<SteerUlwLoopResult> {
        let scope = self.scope();
        match steer_ulw_loop(&self.context.cwd, args, Some(&scope), Some(self.context.surface)) {
            Ok(result) => {
                let resp = ToolkitResponse::Success {
                    ok: true,
                    operation: "steer".to_string(),
                    result,
                    next_actions: Vec::new(),
                    warnings: Vec::new(),
                };
                self.notify("steer", &resp);
                resp
            }
            Err(err) => {
                let resp = ToolkitResponse::Failure {
                    ok: false,
                    operation: "steer".to_string(),
                    error: error_to_toolkit_error(&err),
                    warnings: Vec::new(),
                };
                self.notify("steer", &resp);
                resp
            }
        }
    }

    pub fn add_goal(&self, args: &AddGoalOptions) -> ToolkitResponse<AddGoalResult> {
        let scope = self.scope();
        match add_ulw_loop_goal(&self.context.cwd, args, Some(&scope), Some(self.context.surface)) {
            Ok(result) => {
                let resp = ToolkitResponse::Success {
                    ok: true,
                    operation: "add-goal".to_string(),
                    result,
                    next_actions: Vec::new(),
                    warnings: Vec::new(),
                };
                self.notify("add-goal", &resp);
                resp
            }
            Err(err) => {
                let resp = ToolkitResponse::Failure {
                    ok: false,
                    operation: "add-goal".to_string(),
                    error: error_to_toolkit_error(&err),
                    warnings: Vec::new(),
                };
                self.notify("add-goal", &resp);
                resp
            }
        }
    }

    pub fn criteria(&self, goal_id: &str) -> ToolkitResponse<CriteriaResult> {
        let scope = self.scope();
        let plan_res = read_ulw_loop_plan(&self.context.cwd, Some(&scope));
        match plan_res {
            Ok(plan) => {
                if let Some(goal) = plan.goals.iter().find(|g| g.id == goal_id) {
                    let result = CriteriaResult {
                        goal_id: goal.id.clone(),
                        criteria: goal.success_criteria.clone(),
                    };
                    let resp = ToolkitResponse::Success {
                        ok: true,
                        operation: "criteria".to_string(),
                        result,
                        next_actions: Vec::new(),
                        warnings: Vec::new(),
                    };
                    self.notify("criteria", &resp);
                    resp
                } else {
                    let err = UlwLoopError::new(format!("Unknown ulw-loop id: {goal_id}."), "ULW_LOOP_GOAL_NOT_FOUND");
                    let resp = ToolkitResponse::Failure {
                        ok: false,
                        operation: "criteria".to_string(),
                        error: error_to_toolkit_error(&err),
                        warnings: Vec::new(),
                    };
                    self.notify("criteria", &resp);
                    resp
                }
            }
            Err(err) => {
                let resp = ToolkitResponse::Failure {
                    ok: false,
                    operation: "criteria".to_string(),
                    error: error_to_toolkit_error(&err),
                    warnings: Vec::new(),
                };
                self.notify("criteria", &resp);
                resp
            }
        }
    }

    pub fn record_evidence(&self, args: &RecordEvidenceOptions) -> ToolkitResponse<RecordEvidenceResult> {
        let scope = self.scope();
        match record_evidence(&self.context.cwd, args, Some(&scope)) {
            Ok(result) => {
                let resp = ToolkitResponse::Success {
                    ok: true,
                    operation: "record-evidence".to_string(),
                    result,
                    next_actions: Vec::new(),
                    warnings: Vec::new(),
                };
                self.notify("record-evidence", &resp);
                resp
            }
            Err(err) => {
                let resp = ToolkitResponse::Failure {
                    ok: false,
                    operation: "record-evidence".to_string(),
                    error: error_to_toolkit_error(&err),
                    warnings: Vec::new(),
                };
                self.notify("record-evidence", &resp);
                resp
            }
        }
    }

    pub fn record_review_blockers(&self, args: &RecordFinalReviewBlockersOptions) -> ToolkitResponse<RecordFinalReviewBlockersResult> {
        let scope = self.scope();
        match record_final_review_blockers(&self.context.cwd, args, Some(&scope)) {
            Ok(result) => {
                let next_actions = result.next_actions.clone();
                let warnings = result.warnings.clone();
                let resp = ToolkitResponse::Success {
                    ok: true,
                    operation: "record-review-blockers".to_string(),
                    result,
                    next_actions,
                    warnings,
                };
                self.notify("record-review-blockers", &resp);
                resp
            }
            Err(err) => {
                let resp = ToolkitResponse::Failure {
                    ok: false,
                    operation: "record-review-blockers".to_string(),
                    error: error_to_toolkit_error(&err),
                    warnings: Vec::new(),
                };
                self.notify("record-review-blockers", &resp);
                resp
            }
        }
    }

    pub fn dispatch(&self, request: &Value) -> ToolkitResponse<Value> {
        let op = request.get("operation").and_then(|v| v.as_str()).unwrap_or("");
        if !ULW_LOOP_OPERATIONS.contains(&op) {
            let err = UlwLoopError::new(format!("Unknown operation: {op}"), "ULW_LOOP_OPERATION_UNKNOWN");
            let resp = ToolkitResponse::Failure {
                ok: false,
                operation: op.to_string(),
                error: error_to_toolkit_error(&err),
                warnings: Vec::new(),
            };
            self.notify(op, &resp);
            return resp;
        }

        let args = request.get("args").unwrap_or(&Value::Null);

        match op {
            "help" => {
                let r = self.help();
                match r {
                    ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                        ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                    }
                    ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                }
            }
            "create-goals" => {
                if let Ok(opts) = serde_json::from_value::<CreateGoalsOptions>(args.clone()) {
                    let r = self.create_goals(&opts);
                    match r {
                        ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                            ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                        }
                        ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                    }
                } else {
                    let err = UlwLoopError::new("Invalid args for create-goals", "ULW_LOOP_ARGUMENT_INVALID");
                    ToolkitResponse::Failure { ok: false, operation: op.to_string(), error: error_to_toolkit_error(&err), warnings: Vec::new() }
                }
            }
            "status" => {
                let r = self.status();
                match r {
                    ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                        ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                    }
                    ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                }
            }
            "complete-goals" => {
                let opts = serde_json::from_value::<CompleteGoalsArgs>(args.clone()).ok();
                let r = self.complete_goals(opts.as_ref());
                match r {
                    ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                        ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                    }
                    ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                }
            }
            "checkpoint" => {
                if let Ok(opts) = serde_json::from_value::<CheckpointArgs>(args.clone()) {
                    self.checkpoint(&opts)
                } else {
                    let err = UlwLoopError::new("Invalid args for checkpoint", "ULW_LOOP_ARGUMENT_INVALID");
                    ToolkitResponse::Failure { ok: false, operation: op.to_string(), error: error_to_toolkit_error(&err), warnings: Vec::new() }
                }
            }
            "steer" => {
                if let Ok(opts) = serde_json::from_value::<UlwLoopSteeringProposal>(args.clone()) {
                    let r = self.steer(&opts);
                    match r {
                        ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                            ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                        }
                        ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                    }
                } else {
                    let err = UlwLoopError::new("Invalid args for steer", "ULW_LOOP_ARGUMENT_INVALID");
                    ToolkitResponse::Failure { ok: false, operation: op.to_string(), error: error_to_toolkit_error(&err), warnings: Vec::new() }
                }
            }
            "add-goal" => {
                if let Ok(opts) = serde_json::from_value::<AddGoalOptions>(args.clone()) {
                    let r = self.add_goal(&opts);
                    match r {
                        ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                            ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                        }
                        ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                    }
                } else {
                    let err = UlwLoopError::new("Invalid args for add-goal", "ULW_LOOP_ARGUMENT_INVALID");
                    ToolkitResponse::Failure { ok: false, operation: op.to_string(), error: error_to_toolkit_error(&err), warnings: Vec::new() }
                }
            }
            "criteria" => {
                let gid = args.get("goalId").and_then(|v| v.as_str()).unwrap_or("");
                let r = self.criteria(gid);
                match r {
                    ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                        ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                    }
                    ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                }
            }
            "record-evidence" => {
                if let Ok(opts) = serde_json::from_value::<RecordEvidenceOptions>(args.clone()) {
                    let r = self.record_evidence(&opts);
                    match r {
                        ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                            ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                        }
                        ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                    }
                } else {
                    let err = UlwLoopError::new("Invalid args for record-evidence", "ULW_LOOP_ARGUMENT_INVALID");
                    ToolkitResponse::Failure { ok: false, operation: op.to_string(), error: error_to_toolkit_error(&err), warnings: Vec::new() }
                }
            }
            "record-review-blockers" => {
                if let Ok(opts) = serde_json::from_value::<RecordFinalReviewBlockersOptions>(args.clone()) {
                    let r = self.record_review_blockers(&opts);
                    match r {
                        ToolkitResponse::Success { ok, operation, result, next_actions, warnings } => {
                            ToolkitResponse::Success { ok, operation, result: serde_json::to_value(result).unwrap_or(Value::Null), next_actions, warnings }
                        }
                        ToolkitResponse::Failure { ok, operation, error, warnings } => ToolkitResponse::Failure { ok, operation, error, warnings },
                    }
                } else {
                    let err = UlwLoopError::new("Invalid args for record-review-blockers", "ULW_LOOP_ARGUMENT_INVALID");
                    ToolkitResponse::Failure { ok: false, operation: op.to_string(), error: error_to_toolkit_error(&err), warnings: Vec::new() }
                }
            }
            _ => {
                let err = UlwLoopError::new(format!("Unhandled operation: {op}"), "ULW_LOOP_OPERATION_UNHANDLED");
                ToolkitResponse::Failure { ok: false, operation: op.to_string(), error: error_to_toolkit_error(&err), warnings: Vec::new() }
            }
        }
    }
}
