use std::{collections::BTreeMap,path::PathBuf};
use memory_core::reflection::worktree::ReflectionWorktree;
use super::run_artifacts::RunAttempt;
pub use super::memory_model_attempts::ReflectionChildResult;
#[derive(Clone,Debug)]pub struct ReflectionSpawnPaths{pub session_dir:PathBuf,pub worktree:PathBuf,pub git_common_dir:PathBuf,pub transcript:PathBuf,pub persona:PathBuf,pub prompt:PathBuf,pub skills_usage:Option<PathBuf>,pub dream_state:Option<PathBuf>,pub dream_policy:Option<PathBuf>,pub dream_target:Option<PathBuf>}
#[derive(Clone,Debug)]pub struct DreamPeoplePolicy{pub enabled:bool,pub max_entries:usize,pub max_entry_chars:usize}
pub struct ReflectionSpawnArgs{pub parent_session_file:Option<PathBuf>,pub run_id:Option<String>,pub attempt:u32,pub hard_deadline_at:f64,pub category:String,pub conversation_ids:Vec<String>,pub model:String,pub thinking:Option<String>,pub next_attempt:Option<RunAttempt>,pub kind:Option<String>,pub trigger:Option<String>,pub origin:Option<String>,pub merge_policy:Option<String>,pub target_doc:Option<String>,pub worktree:Option<ReflectionWorktree>,pub command:String,pub args:Vec<String>,pub cwd:PathBuf,pub env:BTreeMap<String,String>,pub paths:ReflectionSpawnPaths}
pub struct FactsSpawnPaths{pub run_dir:PathBuf,pub payload:PathBuf,pub extraction:PathBuf}
pub struct FactsSpawnArgs{pub run_id:String,pub attempt:u32,pub hard_deadline_at:f64,pub model:String,pub thinking:Option<String>,pub next_attempt:Option<RunAttempt>,pub command:String,pub args:Vec<String>,pub cwd:PathBuf,pub env:BTreeMap<String,String>,pub paths:FactsSpawnPaths}
