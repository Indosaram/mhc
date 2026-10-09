//! Checkpoint and continue operation for ulw-loop.

use std::path::Path;
use serde::{Deserialize, Serialize};

use crate::checkpoint::{checkpoint_ulw_loop, CheckpointUlwLoopArgs, CheckpointUlwLoopResult};
use crate::checkpoint_template::checkpoint_template;
use crate::codex_goal_instruction::{build_codex_goal_instruction, BuildCodexGoalInstructionArgs, UlwLoopGoalInstruction};
use crate::domain_types::{UlwLoopItem, UlwLoopPlan};
use crate::paths::UlwLoopScope;
use crate::plan_crud::{start_next_ulw_loop, StartNextResult};
use crate::runtime::UlwLoopError;
use crate::cli_output::blocked_decision_handoff;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContinuationNext {
    Resumed {
        resumed: bool,
        goal: UlwLoopItem,
        instruction: UlwLoopGoalInstruction,
    },
    Done {
        done: bool,
        blocked: bool,
        handoff: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointAndContinueResult {
    #[serde(flatten)]
    pub checkpoint: CheckpointUlwLoopResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<ContinuationNext>,
}

fn done_next(plan: &UlwLoopPlan) -> ContinuationNext {
    let handoff = blocked_decision_handoff(plan);
    ContinuationNext::Done {
        done: true,
        blocked: !handoff.is_empty(),
        handoff,
    }
}

pub fn checkpoint_and_continue(
    repo_root: impl AsRef<Path>,
    args: &CheckpointUlwLoopArgs,
    advance: bool,
    scope: Option<&UlwLoopScope>,
) -> Result<CheckpointAndContinueResult, UlwLoopError> {
    let result = checkpoint_ulw_loop(&repo_root, args, scope, None)?;
    if args.status != "complete" || result.aggregate_completion.is_some() || !advance {
        return Ok(CheckpointAndContinueResult {
            checkpoint: result,
            next: None,
        });
    }

    let next_res = start_next_ulw_loop(&repo_root, None, scope)?;
    match next_res {
        StartNextResult::Done { plan } => {
            let next = done_next(&plan);
            let mut checkpoint = result;
            checkpoint.plan = plan;
            Ok(CheckpointAndContinueResult {
                checkpoint,
                next: Some(next),
            })
        }
        StartNextResult::Started {
            plan,
            goal,
            resumed,
            ..
        } => {
            let instruction = build_codex_goal_instruction(BuildCodexGoalInstructionArgs {
                plan: &plan,
                goal: &goal,
                is_final: None,
                surface: None,
            });
            let next = ContinuationNext::Resumed {
                resumed,
                goal,
                instruction,
            };
            let mut checkpoint = result;
            checkpoint.plan = plan;
            Ok(CheckpointAndContinueResult {
                checkpoint,
                next: Some(next),
            })
        }
    }
}
