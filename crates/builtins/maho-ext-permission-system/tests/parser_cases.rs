use maho_ext_permission_system::parsers::{create_builtin_parser_registry, ParserRegistry, PermissionRequest};
use serde_json::json;
use std::{path::Path, sync::Arc};

fn request(permission: &str, patterns: &[&str], always: &[&str]) -> PermissionRequest {
    PermissionRequest { permission: permission.into(), patterns: patterns.iter().map(|s| (*s).into()).collect(), always: always.iter().map(|s| (*s).into()).collect() }
}
#[test]
fn registered_parser_replaces_fallback() {
    let mut registry = ParserRegistry::default();
    let expected = vec![request("custom", &["alpha"], &["beta"])];
    let returned = expected.clone();
    registry.register("custom_tool", Arc::new(move |_,_,_| returned.clone()));
    assert_eq!(registry.parse("custom_tool", &json!({}), (Path::new("/workspace/project"),Path::new("/home/user"))), expected);
}
#[test]
fn upstream_direct_parser_cases() {
    let registry = create_builtin_parser_registry();
    let cases = [
        ("bash", json!({"command":"git commit -m \"test\""}), vec![request("bash", &["git commit"], &["git commit","git commit *"])]),
        ("bash", json!({"command":"  docker   compose   up  -d  "}), vec![request("bash", &["docker compose up"], &["docker compose up","docker compose up *"])]),
        ("bash", json!({"command":"custom-script --flag value"}), vec![request("bash", &["custom-script"], &["custom-script","custom-script *"])]),
        ("bash", json!({}), vec![request("bash", &["*"], &["*"])]),
        ("bash", json!({"command":"cat /Users/other/project/file.txt"}), vec![request("bash", &["cat"], &["cat","cat *"]),request("external_directory", &["/Users/other/project/file.txt"], &["/Users/other/project/*"])]),
        ("bash", json!({"command":"cp /Users/other/file1.txt /Users/other/file2.txt ."}), vec![request("bash", &["cp"], &["cp","cp *"]),request("external_directory", &["/Users/other/file1.txt","/Users/other/file2.txt"], &["/Users/other/*","/Users/other/*"])]),
        ("bash", json!({"command":"cat ~/other-project/file.txt"}), vec![request("bash", &["cat"], &["cat","cat *"]),request("external_directory", &["~/other-project/file.txt"], &["~/other-project/*"])]),
        ("bash", json!({"command":"cat \"/Users/other/project/file with spaces.txt\""}), vec![request("bash", &["cat"], &["cat","cat *"]),request("external_directory", &["/Users/other/project/file with spaces.txt"], &["/Users/other/project/*"])]),
        ("bash", json!({"command":"cat ./src/index.ts"}), vec![request("bash", &["cat"], &["cat","cat *"])]),
        ("edit", json!({"path":"src/index.ts","edits":[]}), vec![request("edit", &["src/index.ts"], &["src/index.ts"])]),
        ("write", json!({"path":"src/index.ts","content":"hello"}), vec![request("edit", &["src/index.ts"], &["src/index.ts"])]),
        ("apply_patch", json!({"file_path":"src/index.ts"}), vec![request("edit", &["src/index.ts"], &["src/index.ts"])]),
        ("multiedit", json!({"file_path":"src/index.ts"}), vec![request("edit", &["src/index.ts"], &["src/index.ts"])]),
        ("write", json!({"content":"hello"}), vec![request("edit", &["*"], &["*"])]),
        ("read", json!({"path":"README.md"}), vec![request("read", &["README.md"], &["README.md"])]),
        ("read", json!({"file_path":"README.md"}), vec![request("read", &["README.md"], &["README.md"])]),
        ("read", json!({"offset":10}), vec![request("read", &["*"], &["*"])]),
        ("grep", json!({"pattern":"function","path":"src"}), vec![request("grep", &["src"], &["*"])]),
        ("grep", json!({"pattern":"function"}), vec![request("grep", &["function"], &["*"])]),
        ("grep", json!({}), vec![request("grep", &["*"], &["*"])]),
        ("find", json!({"pattern":"**/*.ts","path":"src"}), vec![request("list", &["src"], &["src"])]),
        ("find", json!({"pattern":"**/*.ts"}), vec![request("list", &["."], &["."])]),
        ("ls", json!({"path":"packages","limit":20}), vec![request("list", &["packages"], &["packages"])]),
        ("ls", json!({"limit":20}), vec![request("list", &["."], &["."])]),
    ];
    for (name,input,expected) in cases {
        assert_eq!(registry.parse(name,&input,(Path::new("/Users/me/project"),Path::new("/Users/me"))),expected,"{name}: {input}");
    }
}
