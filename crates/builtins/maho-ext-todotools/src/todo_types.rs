// Copyright (c) 2025 Mario Zechner; Copyright (c) 2025-2026 Can Bölük.
// Adapted from oh-my-pi's MIT-licensed todo tool via senpi.
use serde::{Deserialize,Serialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum TodoStatus { Pending, InProgress, Completed, Abandoned }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum TodoOperation { Init, Start, Done, Rm, Drop, Append, View }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct TodoItem { pub content:String, pub status:TodoStatus }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct TodoPhase { pub name:String, pub tasks:Vec<TodoItem> }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct TodoCompletionTransition { pub phase:String, pub content:String }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum TodoStorage { Session, Memory }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TodoToolDetails {
    #[serde(skip_serializing_if="Option::is_none")]
    pub op:Option<TodoOperation>,
    pub phases:Vec<TodoPhase>,
    pub storage:TodoStorage,
    #[serde(skip_serializing_if="Option::is_none")]
    pub corrections:Option<Vec<String>>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub completed_tasks:Option<Vec<TodoCompletionTransition>>,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub enum TodoStateSchema { #[serde(rename="v2")] V2 }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct TodoStateEntry { pub schema:TodoStateSchema, pub phases:Vec<TodoPhase> }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct TodoPhaseInput { pub phase:String, pub items:Vec<String> }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct TodoOpEntry { pub op:TodoOperation, pub list:Option<Vec<TodoPhaseInput>>, pub task:Option<String>, pub phase:Option<String>, pub items:Option<Vec<String>> }
pub const TODO_STATE_ENTRY_TYPE:&str="senpi.todo-state";
pub const DEFAULT_INIT_PHASE:&str="Tasks";
