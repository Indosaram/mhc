use serde::{Serialize,Deserialize};
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct HistoryEntry {pub text:String,pub session_id:String,pub session_file:String,pub cwd:String,pub timestamp:i64}
