//! Exhaustive built-in path classifications; unknown tools remain unclassified.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum MovedPathToolClass {FilesystemPolicy,Patch,Command{field:&'static str},Paths{write:&'static [&'static str],read:&'static [&'static str]},None}
pub fn moved_path_tool_class(name:&str)->Option<MovedPathToolClass> {
 use MovedPathToolClass::*;
 Some(match name {
 "read"|"write"|"edit"|"find"|"glob"|"ls"|"grep"=>FilesystemPolicy,
 "apply_patch"=>Patch,
 "bash"|"monitor"|"powershell"=>Command{field:"command"},
 "eval"=>Command{field:"code"},"bash_input"=>Command{field:"input"},
 "generate_image"=>Paths{write:&["output_path"],read:&["reference_image_paths","mask_image_path"]},
 "read_video"=>Paths{write:&[],read:&["path"]},"look_at"=>Paths{write:&[],read:&["file_path","file_paths"]},
 "bash_output"|"bash_resize"|"kill_bash"|"request_user_input"|"ask_user_question"|"todo"|"web_search"|"$web_search"|"tool_search"|"webfetch"|"create_goal"|"update_goal"|"get_goal"|"schedule_wakeup"|"schedule_prompt"|"show_html_page"|"list_relocation_targets"|"relocate_session"=>MovedPathToolClass::None,
 _=>return Option::None,
 })
}
#[cfg(test)]mod tests {#[test]fn relocation_is_path_free(){for name in ["list_relocation_targets","relocate_session"]{assert_eq!(super::moved_path_tool_class(name),Some(super::MovedPathToolClass::None));}assert_eq!(super::moved_path_tool_class("third_party"),None);}}
