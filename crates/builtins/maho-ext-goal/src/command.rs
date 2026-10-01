//! Slash command parsing from goal/command.ts.
use crate::types::GoalStatus;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedGoalCommand { Show, Clear, SetStatus(GoalStatus), SetObjective(String) }
pub fn parse_goal_command(raw_args: &str) -> ParsedGoalCommand {
    let trimmed = raw_args.trim();
    match trimmed.to_lowercase().as_str() {
        "" => ParsedGoalCommand::Show,
        "pause" => ParsedGoalCommand::SetStatus(GoalStatus::Paused),
        "resume" => ParsedGoalCommand::SetStatus(GoalStatus::Active),
        "clear" => ParsedGoalCommand::Clear,
        _ => ParsedGoalCommand::SetObjective(trimmed.into()),
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn maps_bare_input_keywords_and_objectives() {
        let inputs = ["", "  ", "pause", "RESUME", "clear", "Ship it"];
        let outputs = inputs.map(parse_goal_command);
        assert_eq!(outputs, [ParsedGoalCommand::Show, ParsedGoalCommand::Show, ParsedGoalCommand::SetStatus(GoalStatus::Paused), ParsedGoalCommand::SetStatus(GoalStatus::Active), ParsedGoalCommand::Clear, ParsedGoalCommand::SetObjective("Ship it".into())]);
    }
}
