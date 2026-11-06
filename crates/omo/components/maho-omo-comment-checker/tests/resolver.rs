use maho_omo_comment_checker::{
    constants::COMMENT_CHECKER_ENV_KEY, resolver::resolve_with_options,
};
use std::{collections::BTreeMap, path::PathBuf};
#[test]
fn absolute_override_wins() {
    let override_path = if cfg!(windows) {
        "C:\\override"
    } else {
        "/override"
    };
    let env = BTreeMap::from([(COMMENT_CHECKER_ENV_KEY.into(), format!(" {override_path} "))]);
    assert_eq!(
        resolve_with_options(
            &env,
            |_| true,
            || panic!("package"),
            |_| panic!("path"),
            "linux"
        ),
        Some(PathBuf::from(override_path))
    );
}
#[test]
fn relative_override_ignored() {
    let env = BTreeMap::from([(COMMENT_CHECKER_ENV_KEY.into(), "relative".into())]);
    assert_eq!(
        resolve_with_options(
            &env,
            |_| true,
            || Some("/package".into()),
            |_| panic!("path"),
            "linux"
        ),
        Some(PathBuf::from("/package"))
    );
}
#[test]
fn missing_override_falls_through() {
    let env = BTreeMap::from([(COMMENT_CHECKER_ENV_KEY.into(), "/missing".into())]);
    assert_eq!(
        resolve_with_options(
            &env,
            |p| p.to_str() != Some("/missing"),
            || Some("/package".into()),
            |_| None,
            "linux"
        ),
        Some(PathBuf::from("/package"))
    );
}
#[test]
fn package_falls_back_to_path() {
    assert_eq!(
        resolve_with_options(
            &BTreeMap::new(),
            |_| false,
            || Some("/missing".into()),
            |name| {
                assert_eq!(name, "comment-checker");
                Some("/path".into())
            },
            "linux"
        ),
        Some(PathBuf::from("/path"))
    );
}
#[test]
fn windows_uses_exe_name() {
    assert!(
        resolve_with_options(
            &BTreeMap::new(),
            |_| false,
            || None,
            |name| {
                assert_eq!(name, "comment-checker.exe");
                None
            },
            "win32"
        )
        .is_none()
    );
}
#[test]
fn cache_dir_name_defaults_to_the_comment_checker_root_and_can_be_reused() {
    use maho_omo_comment_checker::{
        constants::COMMENT_CHECKER_CACHE_DIR_NAME, resolver::default_comment_checker_cache_dir,
    };
    let env = BTreeMap::from([
        ("XDG_CACHE_HOME".to_owned(), "/xdg".to_owned()),
        ("LOCALAPPDATA".to_owned(), "C:\\Local".to_owned()),
    ]);
    assert_eq!(
        default_comment_checker_cache_dir("linux", Some(&env), None),
        PathBuf::from("/xdg")
            .join(COMMENT_CHECKER_CACHE_DIR_NAME)
            .join("bin")
    );
    assert_eq!(
        default_comment_checker_cache_dir("linux", Some(&env), Some("omo-senpi")),
        PathBuf::from("/xdg").join("omo-senpi").join("bin")
    );
    assert_eq!(
        default_comment_checker_cache_dir("win32", Some(&env), Some("omo-senpi")),
        PathBuf::from("C:\\Local").join("omo-senpi").join("bin")
    );
}
