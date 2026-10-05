use serde::{Deserialize,Serialize};
use std::path::PathBuf;
fn serialize_counter<S:serde::Serializer>(value:&f64,serializer:S)->Result<S::Ok,S::Error>{
    if value.is_finite()&&value.fract()==0.0&&value.abs()<=9_007_199_254_740_991.0{serializer.serialize_i64(*value as i64)}else{serializer.serialize_f64(*value)}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum GoalStatus{Active,Paused,Blocked,Complete}
impl GoalStatus{pub const fn as_str(self)->&'static str{match self{Self::Active=>"active",Self::Paused=>"paused",Self::Blocked=>"blocked",Self::Complete=>"complete"}}}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ModelSettableGoalStatus{Complete,Blocked}
#[derive(Clone,Debug)]
pub struct GoalStoreRef{pub base_dir:PathBuf,pub thread_id:String}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum GoalAccountingMode{Active,ActiveOrBlocked,ActiveOrComplete}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum GoalUpdateSource{Model,User}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Goal{
    pub id:String,pub thread_id:String,pub objective:String,pub status:GoalStatus,
    #[serde(skip_serializing_if="Option::is_none")]
    pub token_budget:Option<u64>,
    #[serde(serialize_with="serialize_counter")]
    pub tokens_used:f64,
    #[serde(serialize_with="serialize_counter")]
    pub time_used_seconds:f64,pub created_at:u64,pub updated_at:u64,
    #[serde(skip_serializing_if="Option::is_none")]
    pub last_started_at:Option<u64>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub blocked_reason:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub blocked_at:Option<u64>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub completed_at:Option<u64>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct GoalFile{pub version:u8,pub goal:Option<Goal>}
#[derive(Clone,Debug,Default)]
pub struct TokenUsageSnapshot{pub input:f64,pub output:f64,pub cache_read:f64,pub cache_write:f64,pub total_tokens:f64}
#[derive(Clone,Debug,Default)]
pub struct GoalUpdate{pub objective:Option<String>,pub status:Option<GoalStatus>,pub reason:Option<String>,pub token_budget:Option<Option<f64>>}
#[derive(Clone,Debug,Serialize)]
#[serde(rename_all="camelCase")]
pub struct GoalToolSnapshot{
    pub thread_id:String,pub objective:String,pub status:GoalStatus,
    #[serde(serialize_with="serialize_counter")]
    pub tokens_used:f64,
    #[serde(serialize_with="serialize_counter")]
    pub time_used_seconds:f64,pub created_at:u64,pub updated_at:u64,
    #[serde(skip_serializing_if="Option::is_none")]
    pub blocked_reason:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub blocked_at:Option<u64>,
}
#[derive(Clone,Debug,Serialize)]
pub struct GoalToolResponse{pub goal:Option<GoalToolSnapshot>}
