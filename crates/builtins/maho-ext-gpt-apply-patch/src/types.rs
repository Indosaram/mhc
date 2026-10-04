#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedPatch {
    Add { file_path:String, content:String },
    Delete { file_path:String },
    Update { file_path:String, move_path:Option<String>, chunks:Vec<PatchChunk> },
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatchChunk { pub change_contexts:Vec<String>, pub old_lines:Vec<String>, pub new_lines:Vec<String>, pub is_end_of_file:bool }
pub type AtomicWriteFuture<'a>=std::pin::Pin<Box<dyn std::future::Future<Output=std::io::Result<()>>+Send+'a>>;
pub trait AtomicWriteOperations:Send+Sync {
    fn write_file<'a>(&'a self,path:&'a std::path::Path,content:&'a [u8])->AtomicWriteFuture<'a>;
    fn rename<'a>(&'a self,from:&'a std::path::Path,to:&'a std::path::Path)->AtomicWriteFuture<'a>;
    fn unlink<'a>(&'a self,path:&'a std::path::Path)->AtomicWriteFuture<'a>;
}

use serde::{Serialize,Deserialize};
pub use maho_ai::apply_patch_wire::ApplyPatchWireMode;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ApplyPatchToolVariant { Freeform, Json }
#[derive(Clone,Debug,Default,PartialEq,Eq)]
pub struct ApplyPatchToolsetState { pub removed_edit_tool_names:Vec<String> }
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct ApplyPatchParams { pub input:String }
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ApplyPatchOperation { Add, Delete, Update }
impl ApplyPatchOperation {
    pub const fn as_str(self)-> &'static str { match self { Self::Add=>"add",Self::Delete=>"delete",Self::Update=>"update" } }
}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ApplyPatchProgress { pub applied:usize,pub failed:usize,pub total:usize }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ApplyPatchPreviewFile {
    pub file_path:String,
    #[serde(skip_serializing_if="Option::is_none")]
    pub move_path:Option<String>,
    pub operation:ApplyPatchOperation,
    #[serde(skip_serializing_if="Option::is_none")]
    pub binary:Option<bool>,
    pub diff:String,
    #[serde(skip_serializing_if="Option::is_none")]
    pub patch:Option<String>,
    pub added:usize,pub removed:usize,
}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct ApplyPatchPreview { pub files:Vec<ApplyPatchPreviewFile>,pub added:usize,pub removed:usize }
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ApplyPatchFailure {
    pub operation_index:usize,pub file_path:String,pub operation:ApplyPatchOperation,pub message:String,
    #[serde(skip_serializing_if="Option::is_none")]
    pub code:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AppliedPatchOperation { pub operation_index:usize,pub preview:ApplyPatchPreviewFile }
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ApplyPatchRecoveryInstructions { pub must_read_files:Vec<String>,pub must_not_read_files:Vec<String>,pub failed_files:Vec<String> }
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ApplyPatchResultDetails { pub fuzz:usize,pub applied_operations:Vec<AppliedPatchOperation> }
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ApplyPatchResult {
    pub summaries:Vec<String>,pub applied_files:Vec<String>,pub failures:Vec<ApplyPatchFailure>,
    pub has_partial_success:bool,pub recovery_instructions:ApplyPatchRecoveryInstructions,pub details:ApplyPatchResultDetails,
}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
pub struct ApplyPatchToolDetails {
    #[serde(skip_serializing_if="Option::is_none")]
    pub preview:Option<ApplyPatchPreview>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub progress:Option<ApplyPatchProgress>,
    #[serde(skip_serializing_if="Option::is_none")]
    pub result:Option<ApplyPatchResult>,
}
