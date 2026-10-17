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
fn read_permission_alias_scopes_external_always_to_parent(){let requests=parse("read_permission",json!({"path":"/etc/config"}));assert_eq!(requests[0].permission,"read");assert_eq!(requests[1].permission,"external_directory");assert_eq!(requests[1].always,vec!["/etc/*"]);}
#[test]
fn external_file(){let r=parse("write",json!({"path":"/etc/config"}));assert_eq!(r[1].permission,"external_directory");assert_eq!(r[1].always,vec!["/etc/*"]);}
#[test]
fn grep_pattern(){assert_eq!(parse("grep",json!({"pattern":"TODO"}))[0].patterns,vec!["TODO"]);}
#[test]
fn grep_path(){let r=parse("grep",json!({"path":"/etc","pattern":"TODO"}));assert_eq!(r[0].patterns,vec!["/etc"]);assert_eq!(r[1].always,vec!["/etc/*"]);}
#[test]
fn grep_empty_path_does_not_fall_back_to_pattern(){
    // Given an explicitly empty path and a nonempty pattern.
    let input=json!({"path":"","pattern":"TODO"});
    // When the grep permission is parsed.
    let requests=parse("grep",input);
    // Then upstream's nullish selection uses the empty path and falls back.
    assert_eq!(requests.len(),1);
    assert_eq!(requests[0].permission,"grep");
    assert_eq!(requests[0].patterns,vec!["*"]);
}
#[test]
fn custom_parser_empty_yields_zero_requests() {
    let mut registry = create_builtin_parser_registry();
    registry.set_definition_resolver(std::sync::Arc::new(|name| {
        if name == "omo_browser_bridge" {
            let mut def = maho_ext_api::ToolDefinition::new("omo_browser_bridge", "desc", json!({}), std::sync::Arc::new(|_| Box::pin(async { panic!() })));
            def.permission_parser = Some(std::sync::Arc::new(|_input, _cwd| Vec::new()));
            Some(def)
        } else {
            None
        }
    }));
    let requests = registry.parse("omo_browser_bridge", &json!({}), (Path::new("/workspace"), Path::new("/home/user")));
    assert!(requests.is_empty());
}
#[test]
fn custom_parser_returns_custom_requests() {
    let mut registry = create_builtin_parser_registry();
    registry.set_definition_resolver(std::sync::Arc::new(|name| {
        if name == "custom_tool" {
            let mut def = maho_ext_api::ToolDefinition::new("custom_tool", "desc", json!({}), std::sync::Arc::new(|_| Box::pin(async { panic!() })));
            def.permission_parser = Some(std::sync::Arc::new(|input, _cwd| {
                let target = input.get("target").and_then(serde_json::Value::as_str).unwrap_or("default");
                vec![maho_ext_api::ToolPermissionRequest::new("custom_perm", vec![target.to_string()], vec![format!("{target}/*")])]
            }));
            Some(def)
        } else {
            None
        }
    }));
    let requests = registry.parse("custom_tool", &json!({"target": "resource"}), (Path::new("/workspace"), Path::new("/home/user")));
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].permission, "custom_perm");
    assert_eq!(requests[0].patterns, vec!["resource"]);
    assert_eq!(requests[0].always, vec!["resource/*"]);
}
#[test]
fn list_default(){for name in ["find","ls"]{let r=parse(name,json!({}));assert_eq!(r[0].permission,"list");assert_eq!(r[0].patterns,vec!["."]);}}
#[test]
fn glob_uses_list_permission(){let r=parse("glob",json!({"path":"src"}));assert_eq!(r[0].permission,"list");assert_eq!(r[0].patterns,vec!["src"]);}
#[test]
fn fallback(){assert_eq!(parse("unknown",json!({}))[0].patterns,vec!["*"]);}
#[test]
fn apply_patch_explicit_file_uses_edit_permission(){
    let requests=parse("apply_patch",json!({"path":"src/file.rs"}));
    assert_eq!(requests[0].permission,"edit");
    assert_eq!(requests[0].patterns,vec!["src/file.rs"]);
}
