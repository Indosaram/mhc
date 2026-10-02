use serde::{Deserialize,Serialize};
use super::tool_request_user_input_option::ToolRequestUserInputOption;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ToolRequestUserInputQuestion {pub id:String,pub header:String,pub question:String,pub is_other:bool,pub is_secret:bool,pub options:Option<Vec<ToolRequestUserInputOption>>}
