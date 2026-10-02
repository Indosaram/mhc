use serde::{Deserialize,Serialize};
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ToolRequestUserInputOption {pub label:String,pub description:String}
