use std::{collections::BTreeMap,path::PathBuf};
use maho_omo_comment_checker::{constants::COMMENT_CHECKER_ENV_KEY,resolver::resolve_with_options};
#[test] fn absolute_override_wins() {let env=BTreeMap::from([(COMMENT_CHECKER_ENV_KEY.into()," /override ".into())]);assert_eq!(resolve_with_options(&env,|_|true,||panic!("package"),|_|panic!("path"),"linux"),Some(PathBuf::from("/override")));}
#[test] fn relative_override_ignored() {let env=BTreeMap::from([(COMMENT_CHECKER_ENV_KEY.into(),"relative".into())]);assert_eq!(resolve_with_options(&env,|_|true,||Some("/package".into()),|_|panic!("path"),"linux"),Some(PathBuf::from("/package")));}
#[test] fn missing_override_falls_through() {let env=BTreeMap::from([(COMMENT_CHECKER_ENV_KEY.into(),"/missing".into())]);assert_eq!(resolve_with_options(&env,|p|p.to_str()!=Some("/missing"),||Some("/package".into()),|_|None,"linux"),Some(PathBuf::from("/package")));}
#[test] fn package_falls_back_to_path() {assert_eq!(resolve_with_options(&BTreeMap::new(),|_|false,||Some("/missing".into()),|name|{assert_eq!(name,"comment-checker");Some("/path".into())},"linux"),Some(PathBuf::from("/path")));}
#[test] fn windows_uses_exe_name() {assert!(resolve_with_options(&BTreeMap::new(),|_|true,||None,|name|{assert_eq!(name,"comment-checker.exe");None},"win32").is_none());}
