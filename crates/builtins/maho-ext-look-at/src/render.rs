use std::sync::LazyLock;
use regex::Regex;
use crate::arguments::LookAtArgs;
static IMAGE_REFERENCE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?i)^\s*(?:\[?Image #([1-9][0-9]*)(?:,[^\]\n]*)?\]?|(?:attachment|image)://([1-9][0-9]*))\s*$").expect("literal pattern"));
fn js_whitespace(c:char)->bool { matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
pub fn path_label(path:&str)->String {
    if let Some(captures)=IMAGE_REFERENCE.captures(path) { return format!("Image #{}",captures.get(1).or(captures.get(2)).expect("image index").as_str()); }
    let path=path.trim_end_matches('/'); path.rsplit('/').next().unwrap_or("").into()
}
pub fn source_labels(args:&LookAtArgs)->Vec<String> {
    let mut sources:Vec<_>=args.file_paths.iter().flatten().chain(args.file_path.iter()).filter(|value|!value.trim_matches(js_whitespace).is_empty()).map(|value|path_label(value)).collect();
    sources.extend(args.image_data_list.iter().flatten().chain(args.image_data.iter()).filter(|value|!value.trim_matches(js_whitespace).is_empty()).map(|_|"base64 input".into())); sources
}
pub fn goal_preview(goal:&str)->String {
    let goal=goal.split(js_whitespace).filter(|part|!part.is_empty()).collect::<Vec<_>>().join(" "); if goal.is_empty() { return "pending".into(); }
    if goal.encode_utf16().count()<=110 { return goal; }
    let mut units=0; let mut preview=String::new(); for character in goal.chars() { if units+character.len_utf16()>109 { break; } units+=character.len_utf16(); preview.push(character); } preview.push('…'); preview
}
pub fn text_content(content:&[serde_json::Value])->&str { content.iter().find(|block|block.get("type").and_then(serde_json::Value::as_str)==Some("text")).and_then(|block|block.get("text")).and_then(serde_json::Value::as_str).unwrap_or("") }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn attachment_labels_and_posix_basename() { assert_eq!(path_label("[Image #2, attached]"),"Image #2"); assert_eq!(path_label("image://3"),"Image #3"); assert_eq!(path_label("/tmp/photo.png/"),"photo.png"); assert_eq!(path_label("\\tmp\\photo.png"),"\\tmp\\photo.png"); }
    #[test] fn labels_preserve_plural_then_singular_order() { let args=LookAtArgs{file_paths:Some(vec!["a".into()," ".into()]),file_path:Some("b".into()),image_data:Some("abc".into()),..Default::default()}; assert_eq!(source_labels(&args),["a","b","base64 input"]); }
    #[test] fn first_text_block_is_authoritative() { assert_eq!(text_content(&[serde_json::json!({"type":"text"}),serde_json::json!({"type":"text","text":"later"})]),""); }
}
