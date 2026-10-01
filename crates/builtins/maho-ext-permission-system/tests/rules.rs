use maho_ext_permission_system::{evaluate::evaluate,wildcard::matches,types::{Action,Rule},cli::*};
fn rule(p:&str,v:&str,a:Action)->Rule { Rule{permission:p.into(),pattern:v.into(),action:a} }
#[test]
fn evaluate_single() { let rules=vec![rule("read","docs/*",Action::Allow)]; let r=evaluate("read","docs/guide.md",&[&rules]); assert_eq!(r.action,Action::Allow); assert_eq!(r.pattern,"docs/*"); }

#[test]
fn evaluate_both() { let rules=vec![rule("write","*.ts",Action::Deny)]; let r=evaluate("read","main.ts",&[&rules]); assert_eq!(r.action,Action::Ask); assert_eq!(r.pattern,"*"); }

#[test]
fn evaluate_last() { let rules=vec![rule("read","*",Action::Deny),rule("read","src/*",Action::Allow)]; let r=evaluate("read","src/main.ts",&[&rules]); assert_eq!(r.action,Action::Allow); assert_eq!(r.pattern,"src/*"); }

#[test]
fn evaluate_nomatch() { let rules=vec![rule("read","docs/*",Action::Allow)]; let r=evaluate("read","src/main.ts",&[&rules]); assert_eq!(r.action,Action::Ask); assert_eq!(r.pattern,"*"); }

#[test]
fn evaluate_merged() { let rules=vec![rule("*","*",Action::Deny),rule("read","src/*",Action::Allow)]; let r=evaluate("read","src/main.ts",&[&rules]); assert_eq!(r.action,Action::Allow); assert_eq!(r.pattern,"src/*"); }

#[test]
fn evaluate_override() { let rules=vec![rule("read","src/*",Action::Deny),rule("read","src/*",Action::Allow)]; let r=evaluate("read","src/main.ts",&[&rules]); assert_eq!(r.action,Action::Allow); assert_eq!(r.pattern,"src/*"); }

#[test]
fn evaluate_earlier() { let rules=vec![rule("read","src/*",Action::Allow),rule("write","src/*",Action::Deny)]; let r=evaluate("read","src/main.ts",&[&rules]); assert_eq!(r.action,Action::Allow); assert_eq!(r.pattern,"src/*"); }

#[test]
fn evaluate_three() { let rules=vec![rule("*","*",Action::Deny),rule("read","src/*",Action::Allow),rule("read","src/private/*",Action::Ask)]; let r=evaluate("read","src/private/secret.ts",&[&rules]); assert_eq!(r.action,Action::Ask); assert_eq!(r.pattern,"src/private/*"); }

#[test]
fn evaluate_all() { let rules=vec![rule("*","*",Action::Deny)]; let r=evaluate("bash","rm -rf /tmp/demo",&[&rules]); assert_eq!(r.action,Action::Deny); assert_eq!(r.pattern,"*"); }

#[test]
fn evaluate_permission_glob() { let rules=vec![rule("*","src/*",Action::Ask)]; let r=evaluate("edit","src/main.ts",&[&rules]); assert_eq!(r.action,Action::Ask); assert_eq!(r.pattern,"src/*"); }

#[test]
fn evaluate_path_glob() { let rules=vec![rule("read","*",Action::Allow)]; let r=evaluate("read","any/path",&[&rules]); assert_eq!(r.action,Action::Allow); assert_eq!(r.pattern,"*"); }

#[test]
fn evaluate_suffix() { let rules=vec![rule("read","*.env",Action::Ask)]; let r=evaluate("read","secret.env",&[&rules]); assert_eq!(r.action,Action::Ask); assert_eq!(r.pattern,"*.env"); }

#[test]
fn evaluate_specific() { let rules=vec![rule("read","*",Action::Deny),rule("read","src/*",Action::Allow)]; let r=evaluate("read","src/main.ts",&[&rules]); assert_eq!(r.action,Action::Allow); assert_eq!(r.pattern,"src/*"); }

#[test]
fn evaluate_late_global() { let rules=vec![rule("read","src/*",Action::Allow),rule("read","*",Action::Deny)]; let r=evaluate("read","src/main.ts",&[&rules]); assert_eq!(r.action,Action::Deny); assert_eq!(r.pattern,"*"); }

#[test]
fn evaluate_empty() { let rules=vec![]; let r=evaluate("read","docs/readme.md",&[&rules]); assert_eq!(r.action,Action::Ask); assert_eq!(r.pattern,"*"); }

#[test]
fn evaluate_wrong_permission() { let rules=vec![rule("write","docs/*",Action::Deny)]; let r=evaluate("read","docs/readme.md",&[&rules]); assert_eq!(r.action,Action::Ask); assert_eq!(r.pattern,"*"); }

#[test]
fn evaluate_wrong_path() { let rules=vec![rule("read","src/*",Action::Allow)]; let r=evaluate("read","docs/readme.md",&[&rules]); assert_eq!(r.action,Action::Ask); assert_eq!(r.pattern,"*"); }

#[test]
fn wildcard_exact(){ assert!(matches("read","read")); assert!(!matches("read","write")); }
#[test]
fn wildcard_star(){ assert!(matches("anthropic-web-search","anthropic-*")); assert!(matches("read","*")); assert!(matches("","*")); }
#[test]
fn wildcard_question(){ assert!(matches("read","rea?")); assert!(matches("read","re??")); assert!(!matches("read","re???")); }
#[test]
fn wildcard_backtrack(){ assert!(matches("hello-world-test","hello*test")); assert!(!matches("hello-world-test","hello?foo*")); }
#[test]
fn cli_simple(){ let r=parse_permission_flag("bash=allow"); assert_eq!(r.len(),1); }

#[test]
fn cli_multiple(){ let r=parse_permission_flag("bash=allow,edit=deny"); assert_eq!(r.len(),2); }

#[test]
fn cli_pattern(){ let r=parse_permission_flag("bash:git *=allow"); assert_eq!(r.len(),1); }

#[test]
fn cli_wildcard(){ let r=parse_permission_flag("*=allow"); assert_eq!(r.len(),1); }

#[test]
fn cli_mixed(){ let r=parse_permission_flag("bash=allow,edit:src/*=deny,write=ask"); assert_eq!(r.len(),3); }

#[test]
fn cli_whitespace(){ let r=parse_permission_flag("bash = allow , edit = deny"); assert_eq!(r.len(),2); }

#[test]
fn cli_empty(){ let r=parse_permission_flag(""); assert_eq!(r.len(),0); }

#[test]
fn cli_invalid(){ let r=parse_permission_flag("bash=allow,invalid,edit=deny"); assert_eq!(r.len(),2); }

#[test]
fn cli_actions(){ let r=parse_permission_flag("bash=allow,edit=deny,write=ask"); assert_eq!(r.len(),3); }

#[test]
fn cli_spaces(){ let r=parse_permission_flag("bash:rm -rf *=deny"); assert_eq!(r.len(),1); }
#[test]
fn preset_names(){ for v in ["full-access","workspace","read-only","ask"] { assert!(parse_permission_preset_flag(v).is_some()); } }
#[test]
fn preset_trim(){ assert_eq!(parse_permission_preset_flag(" workspace "),parse_permission_preset_flag("workspace")); }
#[test]
fn preset_invalid(){ assert!(parse_permission_preset_flag("dangerous").is_none()); }

