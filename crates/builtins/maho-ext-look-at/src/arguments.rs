use serde::{Deserialize,Serialize};
use serde_json::{Value,json};
pub const LOOK_AT_USAGE:&str="Usage:\n- look_at(file_path=\"/path/to/file\", goal=\"what to extract\")\n- look_at(file_paths=[\"/path/to/file-1\", \"/path/to/file-2\"], goal=\"what to extract\")\n- look_at(image_data=\"base64_encoded_data\", goal=\"what to extract\")";
#[derive(Clone,Debug,Default,Serialize,Deserialize,PartialEq,Eq)]
pub struct LookAtArgs {
    #[serde(skip_serializing_if="Option::is_none")] pub file_path:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub file_paths:Option<Vec<String>>,
    #[serde(skip_serializing_if="Option::is_none")] pub image_data:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub image_data_list:Option<Vec<String>>,
    #[serde(default)] pub goal:String,
}
pub struct NormalizedLookAtArgs { pub args:LookAtArgs, pub file_paths_from_singular:bool, pub image_data_list_from_singular:bool }
pub fn normalize_look_at_args(mut args:LookAtArgs,path:Option<String>)->NormalizedLookAtArgs {
    args.file_path=args.file_path.or(path);
    let file_paths_from_singular=args.file_paths.is_none() && args.file_path.as_ref().is_some_and(|s|!s.is_empty());
    let image_data_list_from_singular=args.image_data_list.is_none() && args.image_data.as_ref().is_some_and(|s|!s.is_empty());
    if file_paths_from_singular { args.file_paths=args.file_path.clone().map(|p|vec![p]); }
    if image_data_list_from_singular { args.image_data_list=args.image_data.clone().map(|p|vec![p]); }
    NormalizedLookAtArgs { args,file_paths_from_singular,image_data_list_from_singular }
}
pub fn prepare_look_at_arguments(args:&Value)->Result<Value,serde_json::Error> {
    let input=if args.is_object() { args.clone() } else { json!({}) };
    let mut normalized=normalize_look_at_args(serde_json::from_value(input)?,args.get("path").and_then(Value::as_str).map(str::to_owned));
    if normalized.file_paths_from_singular { normalized.args.file_path=None; }
    if normalized.image_data_list_from_singular { normalized.args.image_data=None; }
    serde_json::to_value(normalized.args)
}
fn remote(value:&str)->bool { let prefix=value.get(..7).unwrap_or(""); prefix.eq_ignore_ascii_case("http://") || value.get(..8).is_some_and(|p|p.eq_ignore_ascii_case("https://")) }
pub fn validate_look_at_args(normalized:&NormalizedLookAtArgs)->Option<String> {
    let args=&normalized.args;
    let has_path=args.file_path.as_ref().is_some_and(|s|!s.is_empty());
    let has_paths=args.file_paths.as_ref().is_some_and(|s|!s.is_empty());
    let has_data=args.image_data.as_ref().is_some_and(|s|!s.is_empty());
    let has_list=args.image_data_list.as_ref().is_some_and(|s|!s.is_empty());
    if has_path && has_paths && !normalized.file_paths_from_singular { return Some("Error: Provide either 'file_path' or 'file_paths', not both.".into()); }
    if has_data && has_list && !normalized.image_data_list_from_singular { return Some("Error: Provide either 'image_data' or 'image_data_list', not both.".into()); }
    if let Some(paths)=&args.file_paths {
        for path in paths {
            if path.is_empty() { return Some("Error: 'file_paths' must contain only non-empty local file paths.".into()); }
            if remote(path) { return Some("Error: Remote URLs are not supported for file_paths. Download the file first or use a local path.".into()); }
        }
    }
    if args.image_data_list.as_ref().is_some_and(|s|s.iter().any(String::is_empty)) { return Some("Error: 'image_data_list' must contain only non-empty Base64 image strings.".into()); }
    if has_path && args.file_path.as_deref().is_some_and(remote) { return Some("Error: Remote URLs are not supported for file_path. Download the file first or use a local path.".into()); }
    if !has_path && !has_paths && !has_data && !has_list { return Some(format!("Error: Must provide at least one of 'file_path', 'file_paths', 'image_data', or 'image_data_list'. {LOOK_AT_USAGE}")); }
    if args.goal.is_empty() { return Some(format!("Error: Missing required parameter 'goal'. {LOOK_AT_USAGE}")); }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn alias_becomes_plural() { assert_eq!(prepare_look_at_arguments(&json!({"path":"a.png","goal":"read"})).unwrap(),json!({"file_paths":["a.png"],"goal":"read"})); }
    #[test] fn singular_image_becomes_plural() { assert_eq!(prepare_look_at_arguments(&json!({"image_data":"abc","goal":"read"})).unwrap(),json!({"image_data_list":["abc"],"goal":"read"})); }
    #[test] fn explicit_plural_conflict_retained() { let n=normalize_look_at_args(LookAtArgs { file_path:Some("a".into()),file_paths:Some(vec!["b".into()]),goal:"read".into(),..Default::default() },None); assert!(validate_look_at_args(&n).is_some()); }
    #[test] fn singular_normalization_not_conflict() { let n=normalize_look_at_args(LookAtArgs { file_path:Some("a".into()),goal:"read".into(),..Default::default() },None); assert!(validate_look_at_args(&n).is_none()); }
    #[test] fn empty_plural_is_not_absent() { let n=normalize_look_at_args(LookAtArgs { file_path:Some("a".into()),file_paths:Some(vec![]),goal:"read".into(),..Default::default() },None); assert!(!n.file_paths_from_singular); assert!(validate_look_at_args(&n).is_none()); }
    #[test] fn remote_url_rejected() { let n=normalize_look_at_args(LookAtArgs { file_paths:Some(vec!["HTTPS://example.com/a".into()]),goal:"read".into(),..Default::default() },None); assert!(validate_look_at_args(&n).is_some()); }
    #[test] fn missing_input() { assert!(validate_look_at_args(&normalize_look_at_args(Default::default(),None)).is_some()); }
    #[test] fn missing_goal() { assert!(validate_look_at_args(&normalize_look_at_args(LookAtArgs{image_data:Some("abc".into()),..Default::default()},None)).is_some()); }
    #[test] fn empty_list_item() { assert!(validate_look_at_args(&normalize_look_at_args(LookAtArgs{image_data_list:Some(vec![String::new()]),goal:"read".into(),..Default::default()},None)).is_some()); }
    #[test] fn primitive_arguments() { assert_eq!(prepare_look_at_arguments(&json!(7)).unwrap(),json!({"goal":""})); }
}
