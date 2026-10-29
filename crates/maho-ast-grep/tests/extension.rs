//! Acceptance tests 1 and 2 of `docs/cleanroom/specs/ast-grep.md` §8: registration.

use maho_ast_grep::extension::{BINARY_NAME, plan};
use maho_ext_api::{McpLifecycle, McpTransport};
use std::path::Path;

const CWD_ENV: &str = "MHC_AST_GREP_PROJECT_CWD";

#[test]
fn t1_binary_present_registers_one_lazy_stdio_server_with_the_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(BINARY_NAME), b"x").expect("write binary");
    let exe = dir.path().join("mhc");
    let declaration = plan(&exe, Path::new("/work/project"), None).expect("declaration");
    assert_eq!(declaration.transport, Some(McpTransport::Stdio));
    assert_eq!(declaration.lifecycle, Some(McpLifecycle::Lazy));
    assert_eq!(declaration.args, Some(vec!["mcp".to_owned()]));
    let expected = dir.path().join(BINARY_NAME).to_string_lossy().into_owned();
    assert_eq!(declaration.command, Some(expected));
    let env = declaration.env.expect("env");
    assert_eq!(env.get(CWD_ENV).map(String::as_str), Some("/work/project"));
}

#[test]
fn t1_missing_binary_or_directory_registers_nothing_and_names_the_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exe = dir.path().join("mhc");
    let message = plan(&exe, Path::new("/work"), None).expect_err("diagnostic");
    assert!(message.contains(&dir.path().join(BINARY_NAME).display().to_string()));

    std::fs::create_dir(dir.path().join(BINARY_NAME)).expect("mkdir");
    let message = plan(&exe, Path::new("/work"), None).expect_err("diagnostic");
    assert!(message.contains(BINARY_NAME));
}

#[test]
fn t2_explicit_project_cwd_is_not_overwritten() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(BINARY_NAME), b"x").expect("write binary");
    let exe = dir.path().join("mhc");
    let declaration = plan(&exe, Path::new("/session/cwd"), Some("/explicit")).expect("declaration");
    let env = declaration.env.expect("env");
    assert_eq!(env.get(CWD_ENV).map(String::as_str), Some("/explicit"));
}
