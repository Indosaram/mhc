use maho_ext_permission_system::parsers::create_builtin_parser_registry;
use serde_json::json;
use std::path::Path;
fn parse(name:&str,input:serde_json::Value)->Vec<maho_ext_permission_system::parsers::PermissionRequest>{create_builtin_parser_registry().parse(name,&input,(Path::new("/workspace/project"),Path::new("/home/user")))}
#[test]
fn bash_prefix(){let r=parse("bash",json!({"command":"git status"}));assert_eq!(r[0].permission,"bash");assert_eq!(r[0].patterns,vec!["git status"]);}
#[test]
fn bash_timeout(){assert_eq!(parse("bash",json!({"command":"sleep 5","timeout":10}))[0].permission,"bash");}
#[test]
fn stdin_class(){assert_eq!(parse("bash_input",json!({"input":"rm -rf /etc"}))[0].permission,"bash");}
#[test]
fn file_tools(){for name in ["edit","write","multiedit","read"]{let r=parse(name,json!({"path":"src/index.ts"}));assert_eq!(r.len(),1);assert_eq!(r[0].patterns,vec!["src/index.ts"]);assert_eq!(r[0].permission,if name=="read"{"read"}else{"edit"});}}
#[test]
fn aliases(){assert_eq!(parse("read",json!({"file_path":"src/foo"}))[0].patterns,vec!["src/foo"]);}
#[test]
fn external_file(){let r=parse("write",json!({"path":"/etc/config"}));assert_eq!(r[1].permission,"external_directory");assert_eq!(r[1].always,vec!["/etc/*"]);}
#[test]
fn grep_pattern(){assert_eq!(parse("grep",json!({"pattern":"TODO"}))[0].patterns,vec!["TODO"]);}
#[test]
fn grep_path(){let r=parse("grep",json!({"path":"/etc","pattern":"TODO"}));assert_eq!(r[0].patterns,vec!["/etc"]);assert_eq!(r[1].always,vec!["/etc/*"]);}
#[test]
fn list_default(){for name in ["find","ls"]{let r=parse(name,json!({}));assert_eq!(r[0].permission,"list");assert_eq!(r[0].patterns,vec!["."]);}}
#[test]
fn fallback(){assert_eq!(parse("unknown",json!({}))[0].patterns,vec!["*"]);}
#[test]
fn apply_patch_explicit_file_uses_edit_permission(){
    let requests=parse("apply_patch",json!({"path":"src/file.rs"}));
    assert_eq!(requests[0].permission,"edit");
    assert_eq!(requests[0].patterns,vec!["src/file.rs"]);
}
