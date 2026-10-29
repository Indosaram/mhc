use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;

pub const KERNEL_TOOL_ERROR_CODES: &[&str] = &["tools_unavailable","invalid_tool_definition","reserved_tool_name","tool_name_collision","kernel_tool_stale","kernel_tool_missing","kernel_tool_failed","kernel_tool_recursion","kernel_tool_host_denied","curated_policy_denied"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelToolErrorCode { ToolsUnavailable, InvalidToolDefinition, ReservedToolName, ToolNameCollision, KernelToolStale, KernelToolMissing, KernelToolFailed, KernelToolRecursion, KernelToolHostDenied, CuratedPolicyDenied }
impl KernelToolErrorCode { pub fn as_str(self) -> &'static str { match self { Self::ToolsUnavailable=>"tools_unavailable",Self::InvalidToolDefinition=>"invalid_tool_definition",Self::ReservedToolName=>"reserved_tool_name",Self::ToolNameCollision=>"tool_name_collision",Self::KernelToolStale=>"kernel_tool_stale",Self::KernelToolMissing=>"kernel_tool_missing",Self::KernelToolFailed=>"kernel_tool_failed",Self::KernelToolRecursion=>"kernel_tool_recursion",Self::KernelToolHostDenied=>"kernel_tool_host_denied",Self::CuratedPolicyDenied=>"curated_policy_denied" } } }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelToolError { pub code: KernelToolErrorCode, pub message: String }
impl fmt::Display for KernelToolError { fn fmt(&self,f:&mut fmt::Formatter<'_>)->fmt::Result { f.write_str(&self.message) } }
impl std::error::Error for KernelToolError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelToolLanguage { Js, Py }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KernelToolDescriptor { pub name:String, pub description:String, pub input_schema:Map<String,Value>, pub language:KernelToolLanguage, pub kernel_generation:i64, pub definition_revision:i64 }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KernelToolInvokeRequest { pub name:String, pub kernel_generation:i64, pub definition_revision:i64, pub args:Value, pub call_id:String }
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KernelToolHostScope { pub allow:Option<Vec<String>>, pub deny:Option<Vec<String>> }
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KernelToolInvokeScope { pub tools:Option<KernelToolHostScope> }
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KernelToolCapabilities { pub invoke_scope: bool }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KernelToolDescribeEntry { pub name:String, pub ok:bool, pub descriptor:Option<KernelToolDescriptor>, pub error:Option<KernelToolDescribeError> }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KernelToolDescribeError { pub code:Option<String>, pub message:String }
pub fn supports_invoke_scope(c: Option<&KernelToolCapabilities>) -> bool { c.is_some_and(|v| v.invoke_scope) }
pub fn is_kernel_tool_descriptor(v:&Value)->bool { let Some(o)=v.as_object() else{return false}; o.get("name").and_then(Value::as_str).is_some_and(|x|!x.is_empty()) && o.get("description").and_then(Value::as_str).is_some() && o.get("input_schema").is_some_and(Value::is_object) && matches!(o.get("language").and_then(Value::as_str),Some("js")|Some("py")) && o.get("kernel_generation").and_then(Value::as_i64).is_some() && o.get("definition_revision").and_then(Value::as_i64).is_some() }
pub fn parse_describe_results(v:&Value)->Result<Vec<KernelToolDescribeEntry>,KernelToolError>{ let Some(a)=v.get("results").and_then(Value::as_array) else{return Err(KernelToolError{code:KernelToolErrorCode::ToolsUnavailable,message:"Parent kernel-tool describe reply is not in the expected shape.".into()})}; a.iter().map(|e|{let Some(o)=e.as_object() else{return Err(KernelToolError{code:KernelToolErrorCode::ToolsUnavailable,message:"Parent kernel-tool describe entry is not in the expected shape.".into()})}; let name=o.get("name").and_then(Value::as_str).ok_or_else(||KernelToolError{code:KernelToolErrorCode::ToolsUnavailable,message:"Parent kernel-tool describe entry is not in the expected shape.".into()})?.to_string(); if o.get("ok")==Some(&Value::Bool(true)){let d=o.get("descriptor").filter(|v|is_kernel_tool_descriptor(v)).and_then(|v|serde_json::from_value(v.clone()).ok()).ok_or_else(||KernelToolError{code:KernelToolErrorCode::ToolsUnavailable,message:format!("Parent kernel tool \"{name}\" returned an unusable descriptor.")})?; Ok(KernelToolDescribeEntry{name,ok:true,descriptor:Some(d),error:None})}else{let er=o.get("error").and_then(|v|serde_json::from_value(v.clone()).ok()).unwrap_or(KernelToolDescribeError{code:None,message:format!("Kernel tool is not defined: {name}")}); Ok(KernelToolDescribeEntry{name,ok:false,descriptor:None,error:Some(er)})}}).collect() }
pub fn kernel_tool_error_message(message: impl ToString)->String { message.to_string() }
