#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedPatch {
    Add { file_path:String, content:String },
    Delete { file_path:String },
    Update { file_path:String, move_path:Option<String>, chunks:Vec<PatchChunk> },
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatchChunk { pub change_contexts:Vec<String>, pub old_lines:Vec<String>, pub new_lines:Vec<String>, pub is_end_of_file:bool }
