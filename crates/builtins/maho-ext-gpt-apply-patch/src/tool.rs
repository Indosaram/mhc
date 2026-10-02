use crate::{types::{ApplyPatchResult,ApplyPatchPreview,ApplyPatchToolDetails},preview_format::truncate_preview,apply::compact_apply_patch_result,recovery::build_partial_failure_text};
pub const APPLY_PATCH_RESULT_PATCH_MAX_BYTES:usize=16*1024;
pub fn retained_patch(patch:Option<&str>)->Option<String> { patch.filter(|patch|patch.len()<=APPLY_PATCH_RESULT_PATCH_MAX_BYTES).map(String::from) }
pub fn applied_preview(result:&ApplyPatchResult)->Option<ApplyPatchPreview> {
    let mut operations:Vec<_>=result.details.applied_operations.iter().collect(); operations.sort_by_key(|operation|operation.operation_index);
    if operations.is_empty() { return None; }
    let files:Vec<_>=operations.into_iter().map(|operation|{ let mut file=operation.preview.clone(); file.diff=truncate_preview(&file.diff); file.patch=retained_patch(file.patch.as_deref()); file }).collect();
    Some(ApplyPatchPreview{added:files.iter().map(|file|file.added).sum(),removed:files.iter().map(|file|file.removed).sum(),files})
}
pub fn execution_result(result:ApplyPatchResult)->(String,ApplyPatchToolDetails) {
    let preview=applied_preview(&result); let text=if result.failures.is_empty() { result.summaries.join("\n") } else { build_partial_failure_text(&result) };
    (text,ApplyPatchToolDetails{preview,result:Some(compact_apply_patch_result(result)),progress:None})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn retention_is_bounded_by_utf8_bytes() { assert!(retained_patch(Some(&"a".repeat(16*1024))).is_some()); assert!(retained_patch(Some(&"é".repeat(8193))).is_none()); assert!(retained_patch(None).is_none()); }
    #[test] fn empty_application_has_no_preview() { let result=ApplyPatchResult::default(); assert_eq!(applied_preview(&result),None); let (_,details)=execution_result(result); assert!(details.result.is_some()); assert_eq!(details.preview,None); }
}
