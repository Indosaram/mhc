use maho_ext_pi_rules::rules::scanner::*;
use std::path::{Path,PathBuf};
fn write(root:&Path,name:&str)->PathBuf {let p=root.join(name); let parent=p.parent().unwrap_or_else(||panic!("fixture path has no parent"));std::fs::create_dir_all(parent).unwrap_or_else(|error|panic!("fixture directory: {error}"));std::fs::write(&p,"rule").unwrap_or_else(|error|panic!("fixture file: {error}"));p}
fn scan(root:&Path)->Vec<ScannedFile>{scan_rule_files(ScanOptions{root_dir:root,excluded_dirs:None,max_depth:None})}
fn file(p:&Path)->ScannedFile{ScannedFile{path:p.into(),real_path:p.into()}}
#[test] fn empty_directory(){let d=tempfile::tempdir().unwrap();let r=scan(d.path());assert!(r.is_empty());}
#[test] fn one_markdown(){let d=tempfile::tempdir().unwrap();let p=write(d.path(),"rule.md");let r=scan(d.path());assert_eq!(r,[file(&p)]);}
#[test] fn accepted_extensions(){let d=tempfile::tempdir().unwrap();let a=write(d.path(),"a.md");let b=write(d.path(),"b.mdc");let r=scan(d.path());assert_eq!(r,[file(&a),file(&b)]);}
#[test] fn unrelated_extensions(){let d=tempfile::tempdir().unwrap();for n in ["note.txt","script.ts","markdown.md.tmp"]{write(d.path(),n);}let r=scan(d.path());assert!(r.is_empty());}
#[test] fn nested_rules(){let d=tempfile::tempdir().unwrap();let a=write(d.path(),"root.md");let b=write(d.path(),"nested/deep/rule.mdc");let r=scan(d.path());assert_eq!(r,[file(&b),file(&a)]);}
#[test] fn excludes_node_modules(){let d=tempfile::tempdir().unwrap();write(d.path(),"node_modules/pkg/rule.md");let r=scan(d.path());assert!(r.is_empty());}
#[test] fn excludes_git(){let d=tempfile::tempdir().unwrap();write(d.path(),".git/hooks/rule.md");let r=scan(d.path());assert!(r.is_empty());}
#[test] fn excludes_build_dist(){let d=tempfile::tempdir().unwrap();write(d.path(),"dist/rule.md");write(d.path(),"build/rule.mdc");let r=scan(d.path());assert!(r.is_empty());}
#[test] fn includes_hidden_directory(){let d=tempfile::tempdir().unwrap();let p=write(d.path(),".config/rule.md");let r=scan(d.path());assert_eq!(r,[file(&p)]);}
#[test] fn symlink_inside(){let d=tempfile::tempdir().unwrap();let p=write(d.path(),"target.md");let link=d.path().join("linked.md");std::os::unix::fs::symlink(&p,&link).unwrap();let r=scan(d.path());assert!(r.contains(&ScannedFile{path:link,real_path:p.canonicalize().unwrap()}));}
#[test] fn external_symlink(){let d=tempfile::tempdir().unwrap();let external=tempfile::tempdir().unwrap();let p=write(external.path(),"external.md");let link=d.path().join("linked.md");std::os::unix::fs::symlink(&p,&link).unwrap();let r=scan(d.path());assert_eq!(r,[ScannedFile{path:link,real_path:p.canonicalize().unwrap()}]);}
#[test] fn cycle_terminates(){let d=tempfile::tempdir().unwrap();let p=write(d.path(),"rule.md");std::os::unix::fs::symlink(d.path(),d.path().join("loop")).unwrap();let r=scan(d.path());assert_eq!(r,[file(&p)]);}
#[test] fn nonexistent_root(){let d=tempfile::tempdir().unwrap();let r=scan(&d.path().join("missing"));assert!(r.is_empty());}
#[test] fn file_root(){let d=tempfile::tempdir().unwrap();let p=write(d.path(),"rule.md");let r=scan(&p);assert!(r.is_empty());}
#[test] fn same_real_path_has_two_entries(){let d=tempfile::tempdir().unwrap();let p=write(d.path(),"target.md");let link=d.path().join("linked.md");std::os::unix::fs::symlink(&p,&link).unwrap();let r=scan(d.path());assert_eq!(r,[ScannedFile{path:link,real_path:p.canonicalize().unwrap()},file(&p)]);}
#[test] fn max_depth(){let d=tempfile::tempdir().unwrap();let p=write(d.path(),"one/shallow.md");write(d.path(),"one/two/deep.md");let r=scan_rule_files(ScanOptions{root_dir:d.path(),excluded_dirs:None,max_depth:Some(1)});assert_eq!(r,[file(&p)]);}
#[test] fn deterministic_sort(){let d=tempfile::tempdir().unwrap();let a=write(d.path(),"alpha.md");let b=write(d.path(),"beta/rule.md");let z=write(d.path(),"zeta.md");let r=scan(d.path());assert_eq!(r,[file(&a),file(&b),file(&z)]);}
