use serde::{Deserialize, Serialize};
use serde_json::Map;
use std::collections::BTreeMap;

pub type WireItem = Map<String, serde_json::Value>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoggedTurn {
    pub turn_id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub status: TurnStatus,
    pub items: Vec<WireItem>,
}
pub struct RecordTurnOptions {
    pub turn_id: String,
    pub started_at: String,
    pub status: Option<TurnStatus>,
    pub completed_at: Option<String>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompleteTurnStatus {
    Completed,
    Failed,
    Interrupted,
}
pub struct CompleteTurnOptions {
    pub status: CompleteTurnStatus,
    pub completed_at: String,
    pub error: Option<String>,
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("Turn not found: {0}")]
pub struct TurnNotFound(pub String);
#[derive(Default)]
pub struct TurnLog {
    turns_by_thread_id: BTreeMap<String, Vec<LoggedTurn>>,
}
fn duration_between(started_at: &str, completed_at: Option<&str>) -> Option<i64> {
    let start = super::js_semantics::date_parse_ms(started_at)?;
    let end = super::js_semantics::date_parse_ms(completed_at?)?;
    Some(end - start)
}
impl TurnLog {
    pub fn record_turn(&mut self, thread_id: &str, turn: RecordTurnOptions) -> LoggedTurn {
        let logged = LoggedTurn {
            turn_id: turn.turn_id,
            duration_ms: duration_between(&turn.started_at, turn.completed_at.as_deref()),
            started_at: turn.started_at,
            completed_at: turn.completed_at,
            error: turn.error,
            status: turn.status.unwrap_or(TurnStatus::Running),
            items: vec![],
        };
        self.turns_by_thread_id
            .entry(thread_id.into())
            .or_default()
            .push(logged.clone());
        logged
    }
    fn turn(&mut self, thread_id: &str, turn_id: &str) -> Result<&mut LoggedTurn, TurnNotFound> {
        self.turns_by_thread_id
            .entry(thread_id.into())
            .or_default()
            .iter_mut()
            .find(|turn| turn.turn_id == turn_id)
            .ok_or_else(|| TurnNotFound(turn_id.into()))
    }
    pub fn append_item(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        item: WireItem,
    ) -> Result<(), TurnNotFound> {
        self.turn(thread_id, turn_id)?.items.push(item);
        Ok(())
    }
    pub fn complete_turn(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        completion: CompleteTurnOptions,
    ) -> Result<(), TurnNotFound> {
        let turn = self.turn(thread_id, turn_id)?;
        turn.status = match completion.status {
            CompleteTurnStatus::Completed => TurnStatus::Completed,
            CompleteTurnStatus::Failed => TurnStatus::Failed,
            CompleteTurnStatus::Interrupted => TurnStatus::Interrupted,
        };
        turn.duration_ms = duration_between(&turn.started_at, Some(&completion.completed_at));
        turn.completed_at = Some(completion.completed_at);
        turn.error = completion.error;
        Ok(())
    }
    pub fn read_turns(&mut self, thread_id: &str) -> Vec<LoggedTurn> {
        self.turns_by_thread_id
            .entry(thread_id.into())
            .or_default()
            .clone()
    }
}
