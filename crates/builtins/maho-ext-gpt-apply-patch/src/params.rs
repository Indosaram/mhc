use crate::types::ApplyPatchParams;
use serde_json::Value;
pub fn normalize_apply_patch_arguments(args:&Value)->ApplyPatchParams {
    ApplyPatchParams{input:args.as_str().or_else(||args.get("input").and_then(Value::as_str)).unwrap_or("").into()}
}
#[cfg(test)]
mod tests {
    use super::*; use serde_json::json;
    #[test] fn string_and_input_object_use_verbatim_patch() { for args in [json!("patch\n"),json!({"input":"patch\n"})] { assert_eq!(normalize_apply_patch_arguments(&args).input,"patch\n"); } }
    #[test] fn malformed_arguments_become_empty_input() { for args in [Value::Null,json!(4),json!({"input":false}),json!([])] { assert_eq!(normalize_apply_patch_arguments(&args).input,""); } }
}
