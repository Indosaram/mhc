use maho_ext_permission_system::parsers::{PermissionRequest, create_builtin_parser_registry};
use serde_json::json;
use std::path::Path;

#[test]
fn schema_shaped_inputs_produce_complete_permission_requests() {
    let registry = create_builtin_parser_registry();
    let cwd = Path::new("/workspace/project");
    for (tool, input, permission, pattern, always) in [
        ("bash",json!({"command":"git status"}),"bash","git status",vec!["git status","git status *"]),
        ("bash",json!({"command":"sleep 5","timeout":10}),"bash","sleep",vec!["sleep","sleep *"]),
        ("edit",json!({"path":"src/index.ts","edits":[{"oldText":"foo","newText":"bar"}]}),"edit","src/index.ts",vec!["src/index.ts"]),
        ("write",json!({"path":"src/new-file.ts","content":"export const x = 1;"}),"edit","src/new-file.ts",vec!["src/new-file.ts"]),
        ("read",json!({"path":"README.md"}),"read","README.md",vec!["README.md"]),
        ("read",json!({"path":"large-file.txt","offset":100,"limit":50}),"read","large-file.txt",vec!["large-file.txt"]),
        ("grep",json!({"pattern":"function"}),"grep","function",vec!["*"]),
        ("grep",json!({"pattern":"class","path":"src"}),"grep","src",vec!["*"]),
        ("find",json!({"pattern":"**/*.ts","path":"src"}),"list","src",vec!["src"]),
        ("ls",json!({"path":"packages","limit":20}),"list","packages",vec!["packages"]),
        ("ls",json!({}),"list",".",vec!["."]),
        ("apply_patch",json!({"file_path":"src/patched.ts"}),"edit","src/patched.ts",vec!["src/patched.ts"]),
        ("apply_patch",json!({}),"edit","*",vec!["*"]),
        ("multiedit",json!({"file_path":"src/multi.ts"}),"edit","src/multi.ts",vec!["src/multi.ts"]),
        ("multiedit",json!({}),"edit","*",vec!["*"]),
        ("unknown_tool",json!({"someField":"someValue"}),"unknown_tool","*",vec!["*"]),
        ("new_upstream_tool",json!({"command":"do something"}),"new_upstream_tool","*",vec!["*"]),
    ] {
        assert_eq!(registry.parse(tool,&input,(cwd,Path::new("/home/user"))),vec![PermissionRequest {permission:permission.into(),patterns:vec![pattern.into()],always:always.into_iter().map(str::to_owned).collect()}],"{tool} {input}");
    }
}
