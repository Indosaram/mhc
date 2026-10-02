use serde::{Deserialize,Serialize};
use super::tool_request_user_input_question::ToolRequestUserInputQuestion;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ToolRequestUserInputParams {pub thread_id:String,pub turn_id:String,pub item_id:String,pub questions:Vec<ToolRequestUserInputQuestion>,pub auto_resolution_ms:Option<f64>}
