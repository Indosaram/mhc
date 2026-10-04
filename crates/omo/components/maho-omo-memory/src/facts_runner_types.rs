use serde::{Deserialize,Serialize};
use crate::facts_failure_recording::FactsQueuedKey;
use memory_core::facts::mutation_plan::FactsApplyRecovery;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="status",rename_all="snake_case")]
pub enum FactsLaunchResult{Empty,Active,Skipped,Committed{#[serde(rename="runId")]run_id:String,sha:String},NoFacts{#[serde(rename="runId")]run_id:String},Failed{#[serde(rename="runId")]run_id:String},ParentDirty{#[serde(rename="runId")]run_id:String}}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct FactsRunLedger{
    pub version:u32,pub run_id:String,
    #[serde(skip_serializing_if="Option::is_none")]pub attempt:Option<u32>,
    #[serde(skip_serializing_if="Option::is_none")]pub model:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub thinking:Option<String>,
    pub kind:String,pub started_at:String,pub hard_deadline_at:f64,pub termination_grace_ms:f64,pub deadline_at:f64,pub batch_id:String,pub queued:Vec<FactsQueuedKey>,
    #[serde(skip_serializing_if="Option::is_none")]pub head_before_apply:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub apply_recovery:Option<FactsApplyRecovery>,
    #[serde(skip_serializing_if="Option::is_none")]pub pid:Option<u32>,
    pub process_start:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub child_pid:Option<u32>,
    pub child_process_start:Option<String>,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum FactsTerminalOutcome{Committed,NoFacts,Failed,ParentDirty}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct FactsFinalRecord{pub version:u32,pub run_id:String,pub outcome:FactsTerminalOutcome,#[serde(skip_serializing_if="Option::is_none")]pub sha:Option<String>}
