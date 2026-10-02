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

impl EvalDetachedCellState {
    pub fn as_str(self) -> &'static str {
        match self { Self::Queued => "queued", Self::Running => "running", Self::Detached => "detached", Self::Completed => "completed", Self::Failed => "failed", Self::Cancelled => "cancelled" }
    }
}
