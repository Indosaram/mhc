//! Acceptance test 7 (resolver) and the path rules of test 11 (W part).

mod common;

#[cfg(unix)]
#[tokio::test]
async fn t7_override_without_ast_grep_in_its_version_is_skipped() {
    use maho_ast_grep::resolve::{Toolchain, resolve};
    let dir = tempfile::tempdir().expect("tempdir");
    let impostor = common::fake::script(dir.path(), "impostor", "newgrp 1.0 (shadow tools)", "exit 0");
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).expect("mkdir");
    let genuine = common::fake::program(&bin, "ast-grep", "exit 0");
    let toolchain = Toolchain {
        override_path: Some(impostor.to_string_lossy().into_owned()),
        path_dirs: vec![bin],
        platform: "linux".into(),
        arch: "x64".into(),
        ..Toolchain::default()
    };
    let found = resolve(&toolchain).await.expect("resolved");
    assert_eq!(found, genuine);
}

#[tokio::test]
async fn empty_file_and_missing_file_do_not_pass() {
    use maho_ast_grep::resolve::probe;
    let dir = tempfile::tempdir().expect("tempdir");
    let empty = dir.path().join("sg");
    std::fs::write(&empty, b"").expect("write");
    assert!(!probe(&empty).await);
    assert!(!probe(&dir.path().join("absent")).await);
}

#[test]
fn managed_install_comes_before_path_and_brew_comes_last() {
    use maho_ast_grep::resolve::Toolchain;
    let toolchain = Toolchain {
        override_path: Some("/o/sg".into()),
        home: Some("/home/u".into()),
        path_dirs: vec!["/usr/bin".into()],
        brew_dirs: vec!["/opt/homebrew/bin".into()],
        platform: "darwin".into(),
        arch: "arm64".into(),
    };
    let list: Vec<String> = toolchain
        .candidates()
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(list.first().map(String::as_str), Some("/o/sg"));
    assert!(list[1].ends_with(".omo/runtime/ast-grep/darwin-arm64/ast-grep"));
    assert!(list.iter().position(|p| p == "/usr/bin/ast-grep") < list.iter().position(|p| p == "/opt/homebrew/bin/ast-grep"));
    assert!(list.iter().position(|p| p == "/usr/bin/sg") < list.iter().position(|p| p == "/opt/homebrew/bin/ast-grep"));
}

#[test]
fn t11_windows_style_paths_are_relative_even_when_case_differs() {
    use maho_ast_grep::paths::relative_to;
    assert_eq!(relative_to("C:/Work/Proj", r"c:\work\proj\src\a.js"), "src/a.js");
    assert_eq!(relative_to(r"C:\Work\Proj\", "C:/WORK/PROJ/b.js"), "b.js");
    assert_eq!(relative_to("C:/Work/Proj", "D:/other/x.js"), "D:/other/x.js");
    assert_eq!(relative_to("/w/p", "/w/p/src/a.js"), "src/a.js");
    assert_eq!(relative_to("/w/p", "/w/other/a.js"), "/w/other/a.js");
    assert_eq!(relative_to("/w/p", "/w/pp/a.js"), "/w/pp/a.js");
}
