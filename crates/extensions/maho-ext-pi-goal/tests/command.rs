use maho_ext_pi_goal::goal::command::*;
#[test]fn bare_summary(){assert_eq!(parse_goal_command(""),ParsedGoalCommand::Show);}
#[test]fn arbitrary_objective(){let text="ship the Codex style flow --token-budget 88";assert_eq!(parse_goal_command(text),ParsedGoalCommand::SetObjective{objective:text.into()});}
#[test]fn set_not_special(){let text="set up the release";assert_eq!(parse_goal_command(text),ParsedGoalCommand::SetObjective{objective:text.into()});}
#[test]fn reserved_controls(){assert_eq!(parse_goal_command("pause"),ParsedGoalCommand::Pause);assert_eq!(parse_goal_command("resume"),ParsedGoalCommand::Resume);assert_eq!(parse_goal_command("clear"),ParsedGoalCommand::Clear);}
#[test]fn non_control_words(){for text in ["status","complete","help"]{assert_eq!(parse_goal_command(text),ParsedGoalCommand::SetObjective{objective:text.into()});}}
