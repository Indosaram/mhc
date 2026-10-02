use maho_cli::utils::tools_manager::*;
fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::collections::BTreeMap<String, String>) {
    let directory = tempfile::tempdir().expect("create isolated PATH probe root");
    let bin = directory.path().join("path"); std::fs::create_dir(&bin).expect("create isolated PATH probe bin");
    let env = std::collections::BTreeMap::from([("PATH".to_owned(), bin.to_string_lossy().into_owned())]);
    (directory, bin, env)
}
#[cfg(unix)]
#[test]
fn upstream_executable_path_returns_command_name() {
    use std::os::unix::fs::PermissionsExt;
    let (directory, bin, env) = fixture();
    std::fs::write(bin.join("rg"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(bin.join("rg"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(get_tool_path_in(Tool::Rg, &directory.path().join("managed"), &env, "linux"), Some("rg".to_owned()));
}
#[cfg(unix)]
#[test]
fn upstream_nonexecutable_file_is_not_accepted() {
    use std::os::unix::fs::PermissionsExt;
    let (directory, bin, env) = fixture();
    std::fs::write(bin.join("rg"), "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(bin.join("rg"), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(get_tool_path_in(Tool::Rg, &directory.path().join("managed"), &env, "linux"), None);
}
#[test]
fn upstream_empty_path_finds_nothing() {
    let (directory, _, mut env) = fixture(); env.insert("PATH".to_owned(), String::new());
    assert_eq!(get_tool_path_in(Tool::Rg, &directory.path().join("managed"), &env, "linux"), None);
}
#[test]
fn upstream_directory_candidate_is_skipped() {
    let (directory, bin, env) = fixture(); std::fs::create_dir(bin.join("rg")).unwrap();
    assert_eq!(get_tool_path_in(Tool::Rg, &directory.path().join("managed"), &env, "linux"), None);
}
