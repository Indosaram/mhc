//! Epoch-scoped, one-generation context budget reminder lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenBudgetReminderLease {
    pub compaction_epoch: u64,
    pub message: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenBudgetReminderState {
    pub last_fired_epoch: Option<u64>,
    pub lease: Option<TokenBudgetReminderLease>,
}
pub struct TokenBudgetReminderInput {
    pub context_tokens: f64,
    pub context_window: f64,
    pub threshold_tokens: f64,
    pub lead_tokens: f64,
    pub compaction_epoch: u64,
    pub state: TokenBudgetReminderState,
}
#[derive(Debug, PartialEq, Eq)]
pub struct TokenBudgetReminderResult {
    pub message: Option<String>,
    pub next_state: TokenBudgetReminderState,
}
pub fn create_initial_reminder_state() -> TokenBudgetReminderState {
    TokenBudgetReminderState::default()
}
/// The reminder epoch persisted in the branch: the last compaction's id (or `"initial"`), and whether a custom
/// message after it already carries `details.tokenBudgetReminderEpoch` for that epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedReminderEpoch {
    pub epoch: String,
    pub delivered: bool,
}
pub fn get_persisted_reminder_epoch(
    branch: &[maho_ext_api::SessionEntry],
) -> PersistedReminderEpoch {
    let boundary = branch.iter().rposition(|entry| entry.kind == "compaction");
    let epoch = boundary.map_or_else(|| "initial".to_owned(), |index| branch[index].id.clone());
    let delivered = branch[boundary.map_or(0, |index| index + 1)..]
        .iter()
        .any(|entry| {
            entry.kind == "custom_message"
                && entry
                    .data
                    .get("details")
                    .and_then(|details| details.get("tokenBudgetReminderEpoch"))
                    .and_then(serde_json::Value::as_str)
                    == Some(epoch.as_str())
        });
    PersistedReminderEpoch { epoch, delivered }
}
pub fn clear_token_budget_reminder_lease(
    mut state: TokenBudgetReminderState,
) -> TokenBudgetReminderState {
    state.lease = None;
    state
}
pub fn compute_token_budget_reminder(input: TokenBudgetReminderInput) -> TokenBudgetReminderResult {
    let remaining = input.threshold_tokens - input.context_tokens;
    let in_zone = remaining > 0.0 && remaining <= 2.0 * input.lead_tokens;
    if !in_zone || input.state.last_fired_epoch == Some(input.compaction_epoch) {
        return TokenBudgetReminderResult {
            message: None,
            next_state: clear_token_budget_reminder_lease(input.state),
        };
    }
    let message = format!(
        "[context budget] Approximately {remaining} tokens remain before automatic compaction. Wrap up verbose exploration, prefer concise summaries over full dumps, and front-load conclusions."
    );
    TokenBudgetReminderResult {
        message: Some(message.clone()),
        next_state: TokenBudgetReminderState {
            last_fired_epoch: Some(input.compaction_epoch),
            lease: Some(TokenBudgetReminderLease {
                compaction_epoch: input.compaction_epoch,
                message,
            }),
        },
    }
}
