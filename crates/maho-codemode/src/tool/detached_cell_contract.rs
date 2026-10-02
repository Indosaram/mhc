#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvalDetachedCellState { Queued, Running, Detached, Completed, Failed, Cancelled }

#[derive(Clone, Debug, PartialEq)]
pub struct EvalDetachedCellStatusEntry {
    pub cell_id: String,
    pub language: super::types::EvalLanguage,
    pub summary: Option<String>,
    pub started_at_ms: f64,
    pub queued_behind: Option<Vec<String>>,
}

#[derive(Clone, Debug)]
pub struct EvalDetachedCellSnapshot {
    pub cell_id: String,
    pub language: super::types::EvalLanguage,
    pub started_at_ms: f64,
    pub state: EvalDetachedCellState,
    pub queued_behind: Option<Vec<String>>,
    pub output_tail: String,
    pub result: maho_ext_api::AgentToolResult,
    pub state_retained: Option<bool>,
    pub interrupt_note: Option<String>,
    pub hard_limit_seconds: Option<f64>,
    pub run_budget_seconds: Option<f64>,
}

impl EvalDetachedCellState {
    pub fn as_str(self) -> &'static str {
        match self { Self::Queued => "queued", Self::Running => "running", Self::Detached => "detached", Self::Completed => "completed", Self::Failed => "failed", Self::Cancelled => "cancelled" }
    }
}
