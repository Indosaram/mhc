use crate::types::{ApplyPatchResult,ApplyPatchFailure};
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct ApplyPatchError { pub message:String,pub failures:Vec<ApplyPatchFailure>,pub result:ApplyPatchResult }
impl ApplyPatchError {
    pub fn new(message:impl Into<String>,result:ApplyPatchResult)->Self { Self{message:message.into(),failures:result.failures.clone(),result} }
    pub const fn has_partial_success(&self)->bool { self.result.has_partial_success }
}
impl std::fmt::Display for ApplyPatchError { fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str(&self.message) } }
impl std::error::Error for ApplyPatchError {}
