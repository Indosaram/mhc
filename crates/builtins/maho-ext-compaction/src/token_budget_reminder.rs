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
pub fn create_initial_reminder_state() -> TokenBudgetReminderState { TokenBudgetReminderState::default() }
pub fn clear_token_budget_reminder_lease(mut state: TokenBudgetReminderState) -> TokenBudgetReminderState {
    state.lease = None;
    state
}
pub fn compute_token_budget_reminder(input: TokenBudgetReminderInput) -> TokenBudgetReminderResult {
    let remaining = input.threshold_tokens - input.context_tokens;
    let in_zone = remaining > 0.0 && remaining <= 2.0 * input.lead_tokens;
    if !in_zone || input.state.last_fired_epoch == Some(input.compaction_epoch) {
        return TokenBudgetReminderResult { message: None, next_state: clear_token_budget_reminder_lease(input.state) };
    }
    let message = format!("[context budget] Approximately {remaining} tokens remain before automatic compaction. Wrap up verbose exploration, prefer concise summaries over full dumps, and front-load conclusions.");
    TokenBudgetReminderResult {
        message: Some(message.clone()),
        next_state: TokenBudgetReminderState {
            last_fired_epoch: Some(input.compaction_epoch),
            lease: Some(TokenBudgetReminderLease { compaction_epoch: input.compaction_epoch, message }),
        },
    }
}
