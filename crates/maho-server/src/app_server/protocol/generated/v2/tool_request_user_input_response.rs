use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use super::tool_request_user_input_answer::ToolRequestUserInputAnswer;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ToolRequestUserInputResponse {pub answers:BTreeMap<String,ToolRequestUserInputAnswer>}
