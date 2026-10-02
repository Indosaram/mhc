use maho_ext_permission_system::{config::*,types::*};
use serde_json::json;
fn rule(permission:&str,pattern:&str,action:Action)->Rule{Rule{permission:permission.into(),pattern:pattern.into(),action}}
#[test]
fn edit_tools(){assert_eq!(EDIT_TOOLS,["edit","write","apply_patch","multiedit"]);}
#[test]
fn preset_full(){assert_eq!(DEFAULT_PERMISSION_PRESET,PermissionPresetName::FullAccess);assert_eq!(rules_for_preset(DEFAULT_PERMISSION_PRESET),vec![rule("*","*",Action::Allow)]);}
#[test]
fn preset_ask(){assert_eq!(rules_for_preset(PermissionPresetName::Ask),vec![rule("*","*",Action::Ask)]);}
#[test]
fn preset_workspace(){assert_eq!(rules_for_preset(PermissionPresetName::Workspace),vec![rule("*","*",Action::Ask),rule("read","*",Action::Allow),rule("list","*",Action::Allow),rule("grep","*",Action::Allow),rule("edit","*",Action::Allow),rule("bash","*",Action::Allow),rule("external_directory","*",Action::Ask)]);}
#[test]
fn preset_read_only(){let rules=rules_for_preset(PermissionPresetName::ReadOnly);assert_eq!(rules[4],rule("edit","*",Action::Ask));assert_eq!(rules[5],rule("bash","*",Action::Ask));assert_eq!(rules[6],rule("external_directory","*",Action::Ask));}
#[test]
fn expand_values(){for (input,expected) in [("~/projects/foo","/home/user/projects/foo"),("~","/home/user"),("$HOME/projects/foo","/home/user/projects/foo"),("$HOME","/home/user"),("/usr/local/bin","/usr/local/bin"),("./src/index.ts","./src/index.ts")]{assert_eq!(expand(input,"/home/user"),expected);}}
fn value(value:serde_json::Value)->PermissionValue{serde_json::from_value(value).expect("value")}
#[test]
fn flat_config(){assert_eq!(from_config(&vec![("read".into(),value(json!("allow"))),("write".into(),value(json!("deny")))],"/home/user").expect("config"),vec![rule("read","*",Action::Allow),rule("write","*",Action::Deny)]);}
#[test]
fn nested_config_order(){assert_eq!(from_config(&vec![("read".into(),value(json!({"*.md":"allow","*.txt":"ask"}))),("write".into(),value(json!({"~/projects/*":"allow","/etc/*":"deny"})))],"/home/user").expect("config"),vec![rule("read","*.md",Action::Allow),rule("read","*.txt",Action::Ask),rule("write","/home/user/projects/*",Action::Allow),rule("write","/etc/*",Action::Deny)]);}
#[test]
fn nested_home(){assert_eq!(from_config(&vec![("write".into(),value(json!({"~/projects/*":"allow"})))],"/home/user").expect("config"),vec![rule("write","/home/user/projects/*",Action::Allow)]);}
#[test]
fn nested_dollar_home(){assert_eq!(from_config(&vec![("read".into(),value(json!({"$HOME/.config/*":"allow"})))],"/home/user").expect("config"),vec![rule("read","/home/user/.config/*",Action::Allow)]);}
#[test]
fn mixed_config(){assert_eq!(from_config(&vec![("read".into(),value(json!("allow"))),("write".into(),value(json!({"~/projects/*":"ask"})))],"/home/user").expect("config"),vec![rule("read","*",Action::Allow),rule("write","/home/user/projects/*",Action::Ask)]);}
#[test]
fn empty_config(){assert!(from_config(&vec![],"/home/user").expect("config").is_empty());}
#[test]
fn merge_multiple(){let a=vec![rule("read","*",Action::Allow)];let b=vec![rule("write","*",Action::Ask)];assert_eq!(merge(&[&a,&b]),[a,b].concat());}
#[test]
fn merge_empty(){assert!(merge(&[]).is_empty());}
#[test]
fn merge_single(){let a=vec![rule("read","*",Action::Allow)];assert_eq!(merge(&[&a]),a);}
#[test]
fn merge_order(){let a=vec![rule("read","*.ts",Action::Allow),rule("read","*.js",Action::Ask)];let b=vec![rule("write","*.ts",Action::Deny)];let c=vec![rule("bash","*",Action::Ask)];assert_eq!(merge(&[&a,&b,&c]),[a,b,c].concat());}
#[test]
fn disabled_cases(){for (tools,rules,expected) in [
    (vec!["read","write","edit"],vec![rule("read","*",Action::Allow),rule("write","*",Action::Ask)],vec![]),
    (vec!["read","bash","grep"],vec![rule("bash","*",Action::Deny)],vec!["bash"]),
    (vec!["edit","write","apply_patch","multiedit","read"],vec![rule("edit","*",Action::Deny)],vec!["edit","write","apply_patch","multiedit"]),
    (vec!["write","read"],vec![rule("edit","*",Action::Deny)],vec!["write"]),
    (vec!["read","write"],vec![],vec![]),
    (vec!["read_file","write_file","edit_file"],vec![rule("*write*","*",Action::Deny)],vec!["write_file"]),
    (vec!["read","write"],vec![rule("write","*",Action::Allow),rule("read","*",Action::Ask)],vec![]),
    (vec!["read","write"],vec![rule("write","*.txt",Action::Deny)],vec![]),
]{assert_eq!(disabled(&tools.iter().map(|tool|(*tool).into()).collect::<Vec<_>>(),&rules),expected.iter().map(|tool|(*tool).into()).collect());}}
