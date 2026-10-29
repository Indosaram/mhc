use std::{path::{Path,PathBuf},time::SystemTime};
use crate::types::{PanelGoal,PanelGoalStatus};

#[derive(Default)]
pub struct PanelGoalReader {
    stamp:Option<(PathBuf,SystemTime,u64)>,
    goal:Option<PanelGoal>,
}

impl PanelGoalReader {
    pub fn read(&mut self,path:Option<&Path>)->Option<PanelGoal> {
        let path=path?;
        let stamp=std::fs::metadata(path).ok().and_then(|metadata|metadata.modified().ok().map(|modified|(path.to_path_buf(),modified,metadata.len())));
        let Some(stamp)=stamp else {self.stamp=None;self.goal=None;return None;};
        if self.stamp.as_ref()==Some(&stamp) {return self.goal.clone();}
        self.goal=std::fs::read_to_string(path).ok().and_then(|raw|parse_goal(&raw));
        self.stamp=self.goal.as_ref().map(|_|stamp);
        self.goal.clone()
    }
}

pub fn parse_goal(raw:&str)->Option<PanelGoal> {
    let value:serde_json::Value=serde_json::from_str(raw).ok()?;
    let goal=value.get("goal")?.as_object()?;
    let objective=goal.get("objective")?.as_str().filter(|text|!text.trim().is_empty())?.to_owned();
    let status=match goal.get("status")?.as_str()? {
        "active"=>PanelGoalStatus::Active,"paused"=>PanelGoalStatus::Paused,"blocked"=>PanelGoalStatus::Blocked,
        "budgetLimited"=>PanelGoalStatus::BudgetLimited,"complete"=>PanelGoalStatus::Complete,_=>return None,
    };
    let finite=|key:&str|goal.get(key).and_then(serde_json::Value::as_f64).filter(|number|number.is_finite());
    Some(PanelGoal {objective,status,tokens_used:finite("tokensUsed").unwrap_or(0.0),time_used_seconds:finite("timeUsedSeconds").unwrap_or(0.0),
        consecutive_continuations:goal.get("consecutiveContinuations").and_then(serde_json::Value::as_u64).unwrap_or(0),
        unattended_continuations:goal.get("unattendedContinuations").and_then(serde_json::Value::as_u64).unwrap_or(0),token_budget:finite("tokenBudget")})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn partial_or_unknown_goals_do_not_break_the_panel() {
        assert!(parse_goal("{\"goal\":").is_none());
        assert!(parse_goal(r#"{"goal":{"objective":"x","status":"unknown"}}"#).is_none());
        let goal=parse_goal(r#"{"goal":{"objective":"finish","status":"active"}}"#).expect("valid goal");
        assert_eq!(goal.objective,"finish");
        assert!(goal.tokens_used.abs()<f64::EPSILON);
        assert!(goal.token_budget.is_none());
    }
}
