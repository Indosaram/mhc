use senpi_task::completion::{CompletionDetails, completion_message_lines};
use senpi_task::renderer_text::normalize_renderer_text;
use maho_ext_api::{CustomMessage, ExtensionApi, ToolContent};
use maho_tui::tui::Component;
use serde::Deserialize;
use std::sync::Arc;

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

// CompletionDetails is serialize-only in the public engine API. Decode the
// adapter's message envelope here rather than modifying that engine type.
#[derive(Deserialize)]
struct CompletionPayload {
    task_id: String,
    name: String,
    status: String,
    category: Option<String>,
    agent_type: Option<String>,
    model: String,
    requested_model: Option<ModelPayload>,
    fallback_models: Option<Vec<ModelPayload>>,
    resolved_model: Option<ModelPayload>,
    duration_ms: i64,
    tokens: Option<u64>,
    run_stats: Option<senpi_task::state::TaskRunStats>,
    final_response: String,
    final_response_file: Option<String>,
    continuation_hint: String,
}

#[derive(Deserialize)]
struct ModelPayload {
    provider: String, model_id: String, display: String, source: String,
    variant: Option<String>, reasoning_effort: Option<String>, reasoning: Option<String>,
}

impl ModelPayload {
    fn decode(self) -> Option<senpi_task::state::ResolvedModelRecord> {
        Some(senpi_task::state::ResolvedModelRecord {
            source: senpi_task::state::RESOLVED_MODEL_SOURCES.into_iter()
                .find(|source| source.as_str() == self.source)?,
            provider: self.provider, model_id: self.model_id, display: self.display,
            variant: self.variant, reasoning_effort: self.reasoning_effort, reasoning: self.reasoning,
        })
    }
}

impl CompletionPayload {
    fn decode(self) -> Option<CompletionDetails> {
        let value = self;
        Some(CompletionDetails {
            task_id: value.task_id, name: value.name,
            status: senpi_task::state::TaskStatus::parse(&value.status)?,
            category: value.category, agent_type: value.agent_type, model: value.model,
            requested_model: match value.requested_model { Some(model) => Some(model.decode()?), None => None },
            fallback_models: match value.fallback_models { Some(models) => Some(models.into_iter().map(ModelPayload::decode).collect::<Option<Vec<_>>>()?), None => None },
            resolved_model: match value.resolved_model { Some(model) => Some(model.decode()?), None => None }, duration_ms: value.duration_ms,
            tokens: value.tokens, run_stats: value.run_stats, final_response: value.final_response,
            final_response_file: value.final_response_file, continuation_hint: value.continuation_hint,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LivenessPayload {
    member_name: String,
    last_known_state: String,
    reason: Option<String>,
}

enum MessageLines {
    Fixed(Vec<String>),
    Completion(Vec<CompletionDetails>),
}

impl Component for MessageLines {
    fn render(&mut self, width: usize) -> Vec<String> {
        match self {
            Self::Fixed(lines) => lines.clone(),
            Self::Completion(details) => render_task_completion(details, width),
        }
    }
}

/// Register the native equivalents of the three upstream MessageRenderers.
/// Completion lines remain width-dependent, as in linesComponent(callback).
pub fn register_task_message_renderers(api: &mut ExtensionApi) {
    api.register_message_renderer(CATEGORY_UNAVAILABLE_MESSAGE_TYPE, Arc::new(|message, _, _| {
        let content = match message.content.as_slice() {
            [ToolContent::Text { text, .. }] => Some(text.as_str()),
            _ => None,
        };
        Some(Box::new(MessageLines::Fixed(render_category_unavailable(content))))
    }));
    api.register_message_renderer(TASK_COMPLETION_MESSAGE_TYPE, Arc::new(|message, _, _| {
        let details = match &message.details {
            None => Vec::new(),
            Some(value) => serde_json::from_value::<Vec<CompletionPayload>>(value.clone()).ok()?
                .into_iter().map(CompletionPayload::decode).collect::<Option<Vec<_>>>()?,
        };
        Some(Box::new(MessageLines::Completion(details)))
    }));
    api.register_message_renderer(TEAM_MEMBER_LIVENESS_MESSAGE_TYPE, Arc::new(|message: &CustomMessage, _, _| {
        let payload = message.details.as_ref().map(|value| {
            serde_json::from_value::<LivenessPayload>(value.clone())
        }).transpose().ok()?;
        let details = payload.as_ref().map(|value| TeamMemberLivenessDetails {
            member_name: &value.member_name,
            last_known_state: &value.last_known_state,
            reason: value.reason.as_deref(),
        });
        Some(Box::new(MessageLines::Fixed(render_team_member_liveness(details.as_ref()))))
    }));
}
