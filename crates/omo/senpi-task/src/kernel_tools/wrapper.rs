use serde_json::{json,Value};
use super::contract::KernelToolErrorCode;
#[derive(Debug,Clone,PartialEq)]pub struct KernelToolResultDetails{pub kernel_tool:String,pub error:Option<(KernelToolErrorCode,String)>}
pub fn kernel_tool_error_result(name:&str,code:KernelToolErrorCode,message:&str)->Value{json!({"content":[{"type":"text","text":serde_json::to_string(&json!({"kernel_tool":name,"error":{"code":code.as_str(),"message":message}})).unwrap_or_default()}],"details":{"kernel_tool":name,"error":{"code":code.as_str(),"message":message}},"isError":true})}
