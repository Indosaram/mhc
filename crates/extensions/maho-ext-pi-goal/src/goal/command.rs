#[derive(Debug,PartialEq,Eq)]
pub enum ParsedGoalCommand {Show,Clear,Pause,Resume,SetObjective{objective:String}}
pub fn parse_goal_command(raw_args:&str)->ParsedGoalCommand{
    let trimmed=raw_args.trim();
    if trimmed.is_empty(){return ParsedGoalCommand::Show;}
    match trimmed.to_lowercase().as_str(){
        "pause"=>ParsedGoalCommand::Pause,"resume"=>ParsedGoalCommand::Resume,"clear"=>ParsedGoalCommand::Clear,
        _=>ParsedGoalCommand::SetObjective{objective:trimmed.into()},
    }
}
