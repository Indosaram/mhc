use maho_ext_builtin_loose::account_display_name::*;
#[test]
fn rename_preserves_internal_spaces(){let command=parse_display_name_command("  rename  id  My   account  ").expect("grammar").expect("command");assert_eq!(command.account_id,"id");assert_eq!(command.display_name.as_deref(),Some("My   account"));assert_eq!(parse_display_name_command("rename id").expect("grammar").expect("command").display_name.as_deref(),Some(""));}
#[test]
fn clear_and_unknown_actions(){assert_eq!(parse_display_name_command("clear-name id").expect("grammar").expect("command").display_name,None);assert!(parse_display_name_command("clear-name id extra").is_err());assert!(parse_display_name_command("rename").is_err());assert!(parse_display_name_command("list id").expect("grammar").is_none());}
