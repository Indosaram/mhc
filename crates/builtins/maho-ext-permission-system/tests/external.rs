use maho_ext_permission_system::external_dir::{is_external_path,extract_external_paths};
use std::{path::Path,os::unix::fs::{symlink,PermissionsExt}};
#[test]
fn symlinked_cwd(){let root=tempfile::tempdir().expect("dir");let real=root.path().join("real");std::fs::create_dir(&real).expect("mkdir");let link=root.path().join("link");symlink(&real,&link).expect("link");assert!(!is_external_path("src/new.ts",&link,root.path()));}
#[test]
fn execute_only(){let root=tempfile::tempdir().expect("dir");let real=root.path().join("real");let noread=real.join("noread");std::fs::create_dir_all(&noread).expect("mkdir");std::fs::set_permissions(&noread,std::fs::Permissions::from_mode(0o111)).expect("mode");let link=root.path().join("link");symlink(&real,&link).expect("link");let result=is_external_path(&link.join("noread/new.txt").to_string_lossy(),&link,root.path());std::fs::set_permissions(&noread,std::fs::Permissions::from_mode(0o755)).expect("restore");assert!(!result);}
#[test]
fn escaping_link(){let root=tempfile::tempdir().expect("dir");let cwd=root.path().join("project");std::fs::create_dir(&cwd).expect("mkdir");std::fs::create_dir(root.path().join("outside")).expect("mkdir");symlink("../outside",cwd.join("escape")).expect("link");assert!(is_external_path("escape/file.txt",&cwd,root.path()));assert!(!is_external_path("kept/file.txt",&cwd,root.path()));}
#[test]
fn loop_stops(){let root=tempfile::tempdir().expect("dir");symlink("b",root.path().join("a")).expect("link");symlink("a",root.path().join("b")).expect("link");assert!(!is_external_path("a/file.txt",root.path(),root.path()));}
#[test]
fn shell_paths(){assert_eq!(extract_external_paths("cat '/etc/file name' ./inside ../outside --flag=/etc/no ENV=/etc/no",Path::new("/workspace/project"),Path::new("/home/user")),vec!["/etc/file name","../outside"]);}
