use crate::types::{ApplyPatchFailure,ApplyPatchRecoveryInstructions,ApplyPatchResult};
pub fn create_recovery_instructions(applied_files:&[String],failures:&[ApplyPatchFailure])->ApplyPatchRecoveryInstructions {
    let mut must_read_files=vec![]; let mut failed_files=vec![]; let mut must_not_read_files=vec![];
    for failure in failures {
        if failure.code.is_none() && !must_read_files.contains(&failure.file_path) { must_read_files.push(failure.file_path.clone()); }
        if !failed_files.contains(&failure.file_path) { failed_files.push(failure.file_path.clone()); }
    }
    for path in applied_files { if !must_read_files.contains(path) && !must_not_read_files.contains(path) { must_not_read_files.push(path.clone()); } }
    ApplyPatchRecoveryInstructions{must_read_files,must_not_read_files,failed_files}
}
pub fn build_partial_failure_text(result:&ApplyPatchResult)->String {
    let mut lines=vec![if result.has_partial_success { "apply_patch partially failed.".into() } else { "apply_patch failed.".into() },"Failed:".into()];
    lines.extend(result.failures.iter().map(|f|format!("- {} ({}): {}",f.file_path,f.operation.as_str(),f.message)));
    if !result.recovery_instructions.must_read_files.is_empty() { lines.push(format!("Recovery: MUST read {} before retrying.",result.recovery_instructions.must_read_files.join(" and "))); }
    lines.push(if result.applied_files.is_empty() { "No file actions were applied.".into() } else { "Earlier file actions in this patch were already applied.".into() });
    if !result.recovery_instructions.must_not_read_files.is_empty() { lines.push("Recovery: MUST NOT reread other files from this patch unless a specific dependency requires it.".into()); }
    lines.join("\n")
}
#[cfg(test)]
mod tests {
    use super::*; use crate::types::ApplyPatchOperation;
    fn failure(path:&str,code:Option<&str>)->ApplyPatchFailure { ApplyPatchFailure{operation_index:0,file_path:path.into(),operation:ApplyPatchOperation::Update,message:"mismatch".into(),code:code.map(String::from)} }
    #[test] fn only_context_failures_require_reread() { let result=create_recovery_instructions(&["done".into()],&[failure("missing",Some("ENOENT")),failure("context",None)]); assert_eq!(result.must_read_files,vec!["context"]); assert_eq!(result.must_not_read_files,vec!["done"]); assert_eq!(result.failed_files,vec!["missing","context"]); }
    #[test] fn deduplicates_without_reordering() { let result=create_recovery_instructions(&["a".into(),"b".into(),"a".into()],&[failure("b",None),failure("b",None)]); assert_eq!(result.must_read_files,vec!["b"]); assert_eq!(result.must_not_read_files,vec!["a"]); }
}
