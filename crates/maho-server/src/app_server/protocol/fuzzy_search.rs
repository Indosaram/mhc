use serde::{Deserialize,Serialize};
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum FuzzyFileSearchMatchType {File,Directory}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct FuzzyFileSearchResult {pub root:String,pub path:String,pub match_type:FuzzyFileSearchMatchType,pub file_name:String,pub score:f64,#[serde(deserialize_with="super::nullable::deserialize_required")]pub indices:Option<Vec<f64>>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct FuzzyFileSearchParams {pub query:String,pub roots:Vec<String>,#[serde(deserialize_with="super::nullable::deserialize_required")]pub cancellation_token:Option<String>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct FuzzyFileSearchResponse {pub files:Vec<FuzzyFileSearchResult>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct FuzzyFileSearchSessionStartParams {pub session_id:String,pub roots:Vec<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct FuzzyFileSearchSessionUpdateParams {pub session_id:String,pub query:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct FuzzyFileSearchSessionStopParams {pub session_id:String}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct FuzzyFileSearchSessionStartResponse {}
pub type FuzzyFileSearchSessionUpdateResponse = FuzzyFileSearchSessionStartResponse;
pub type FuzzyFileSearchSessionStopResponse = FuzzyFileSearchSessionStartResponse;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct FuzzyFileSearchSessionUpdatedNotification {pub session_id:String,pub query:String,pub files:Vec<FuzzyFileSearchResult>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct FuzzyFileSearchSessionCompletedNotification {pub session_id:String}
