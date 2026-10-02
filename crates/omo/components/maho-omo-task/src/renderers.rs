use senpi_task::completion::{CompletionDetails, completion_message_lines};
use senpi_task::renderer_text::normalize_renderer_text;

pub const TASK_COMPLETION_MESSAGE_TYPE: &str = "senpi-task.completion";
pub const TEAM_MEMBER_LIVENESS_MESSAGE_TYPE: &str = "senpi-task.team-member-liveness";
pub const CATEGORY_UNAVAILABLE_MESSAGE_TYPE: &str = "senpi-task.category-unavailable";

pub fn render_category_unavailable(content: Option<&str>) -> Vec<String> {
    vec![normalize_renderer_text(content.filter(|text| !text.is_empty()).unwrap_or("(category unavailable)"))]
}

pub fn render_task_completion(details: &[CompletionDetails], width: usize) -> Vec<String> {
    if details.is_empty() {
        vec!["(task completion)".into()]
    } else {
        completion_message_lines(details, Some(width))
    }
}

pub struct TeamMemberLivenessDetails<'a> {
    pub member_name: &'a str,
    pub last_known_state: &'a str,
    pub reason: Option<&'a str>,
}

pub fn render_team_member_liveness(details: Option<&TeamMemberLivenessDetails<'_>>) -> Vec<String> {
    let Some(details) = details else { return vec!["(team member liveness)".into()] };
    let mut rows = vec!["team member liveness".into(), format!("member:{}", normalize_renderer_text(details.member_name)), format!("last state:{}", normalize_renderer_text(details.last_known_state))];
    if let Some(reason) = details.reason { rows.push(format!("reason:{}", normalize_renderer_text(reason))); }
    rows
}
