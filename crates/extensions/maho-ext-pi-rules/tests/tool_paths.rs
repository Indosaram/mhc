use maho_ext_pi_rules::rules::tool_paths::*;
use serde_json::{json,Value};
use std::path::{Path,PathBuf};
fn extract(name:&str,error:bool,input:Value,details:Option<Value>)->Vec<PathBuf>{extract_tool_paths(name,error,&input,details.as_ref(),Path::new("/tmp/project"))}
#[test] fn read_details(){let r=extract("read",false,json!({}),Some(json!({"filePath":"/tmp/project/src/read-target.ts"})));assert_eq!(r,[PathBuf::from("/tmp/project/src/read-target.ts")]);}
#[test] fn edit_details(){let r=extract("edit",false,json!({}),Some(json!({"filePath":"/tmp/project/src/edit-target.ts"})));assert_eq!(r,[PathBuf::from("/tmp/project/src/edit-target.ts")]);}
#[test] fn write_input(){let r=extract("write",false,json!({"filePath":"/tmp/project/src/write-target.ts"}),None);assert_eq!(r,[PathBuf::from("/tmp/project/src/write-target.ts")]);}
#[test] fn relative_read(){let r=extract("read",false,json!({"path":"src/read-target.ts"}),None);assert_eq!(r,[PathBuf::from("/tmp/project/src/read-target.ts")]);}
#[test] fn error_ignored(){let r=extract("read",true,json!({}),Some(json!({"filePath":"/tmp/project/a"})));assert!(r.is_empty());}
#[test] fn bash_ignored(){assert!(extract("bash",false,json!({"filePath":"a"}),None).is_empty());}
#[test] fn grep_ignored(){assert!(extract("grep",false,json!({"filePath":"a"}),None).is_empty());}
#[test] fn find_ignored(){assert!(extract("find",false,json!({"filePath":"a"}),None).is_empty());}
#[test] fn ls_ignored(){assert!(extract("ls",false,json!({"filePath":"a"}),None).is_empty());}
#[test] fn custom_ignored(){assert!(extract("custom-tool",false,json!({"filePath":"a"}),None).is_empty());}
#[test] fn missing_details(){assert!(extract("read",false,json!({}),None).is_empty());}
#[test] fn no_filepath(){assert!(extract("read",false,json!({}),Some(json!({"truncation":{"truncated":false}}))).is_empty());}
#[test] fn no_write_input(){assert!(extract("write",false,json!({}),None).is_empty());}
#[test] fn read_tracked(){assert!(is_tracked_tool("read"));}
#[test] fn write_tracked(){assert!(is_tracked_tool("write"));}
#[test] fn edit_tracked(){assert!(is_tracked_tool("edit"));}
#[test] fn bash_not_tracked(){assert!(!is_tracked_tool("bash"));}
