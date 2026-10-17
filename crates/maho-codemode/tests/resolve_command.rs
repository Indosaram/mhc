use maho_codemode::interpreters::resolve_command::*;

#[test]
fn ignores_non_executable_and_directories() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("probe"), "not executable").unwrap();
    let env = std::collections::HashMap::from([("PATH".into(), root.path().to_string_lossy().into_owned())]);
    assert!(resolve_command_path("probe", &env, root.path(), false).is_none());
    assert!(resolve_command_path(&root.path().to_string_lossy(), &env, root.path(), false).is_none());
}

#[cfg(unix)]
#[test]
fn slash_command_normalizes_dot_segments_without_resolving_symlinks() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("bin")).unwrap();
    let executable = root.path().join("bin/probe");
    std::fs::write(&executable, "fixture").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    symlink("bin", root.path().join("alias")).unwrap();
    let env = Default::default();
    assert_eq!(resolve_command_path("./bin/../alias/./probe", &env, root.path(), false), Some(root.path().join("alias/probe")));
}

#[test]
fn windows_extension_candidates_prefer_lowercase_before_original() {
    let root=tempfile::tempdir().unwrap();
    for name in ["probe.exe","probe.EXE"] {std::fs::write(root.path().join(name),"fixture").unwrap();}
    let env=std::collections::HashMap::from([("PATH".into(),root.path().to_string_lossy().into_owned()),("PATHEXT".into(),".EXE".into())]);
    assert_eq!(resolve_command_path("probe",&env,root.path(),true),Some(root.path().join("probe.exe")));
}

#[test]
fn injected_windows_resolution_uses_host_path_delimiter() {
    let root=tempfile::tempdir().unwrap();
    let first=root.path().join("missing");
    std::fs::write(root.path().join("probe.exe"),"fixture").unwrap();
    let path=std::env::join_paths([first.as_path(),root.path()]).unwrap();
    let env=std::collections::HashMap::from([("PATH".into(),path.to_string_lossy().into_owned()),("PATHEXT".into(),".EXE".into())]);
    assert_eq!(resolve_command_path("probe",&env,root.path(),true),Some(root.path().join("probe.exe")));
}

#[cfg(unix)]
#[test]
fn path_directory_trailing_backslash_does_not_add_separator() {
    let root=tempfile::tempdir().unwrap();
    let expected=root.path().join("folder\\probe.exe");
    std::fs::write(&expected,"fixture").unwrap();
    let env=std::collections::HashMap::from([("PATH".into(),format!("{}/folder\\",root.path().display())),("PATHEXT".into(),".EXE".into())]);
    assert_eq!(resolve_command_path("probe",&env,root.path(),true),Some(expected));
}
