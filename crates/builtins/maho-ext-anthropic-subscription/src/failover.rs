//! Port of senpi `anthropic-subscription/failover.ts` (pin 903b8336).
use crate::{
    accounts::{AccountSlot, AccountSource},
    affinity::AllAccountsBlockedError,
    errors::{SdkErrorClassification, SdkErrorKind},
    model_blocks::{
        ModelBlocks, merge_model_blocks, model_block_key, prune_model_blocks, with_model_block,
    },
};
use maho_ai::auth::types::{Credential, CredentialStore};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
pub const MAX_RATE_LIMIT_BLOCK_MS: f64 = 48.0 * 60.0 * 60.0 * 1000.0;
pub const DEFAULT_RATE_LIMIT_BLOCK_MS: f64 = 60000.0;
/// Same-account retries per turn for a transient failure (overload, network, server error); it never rotates.
pub const TRANSIENT_RETRIES_PER_TURN: u32 = 2;
pub const TRANSIENT_RETRY_DELAY_MS: u64 = 1_000;
pub const TURN_RETRY_SUPPRESSION_PREFIX: &str = "senpi:no-turn-retry:";
pub type AttemptFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<Value>, Value>> + Send + 'a>>;
pub trait AttemptMessages: Send {
    fn next(&mut self) -> AttemptFuture<'_>;
}
pub type AttemptFactoryFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Box<dyn AttemptMessages>, Value>> + Send + 'a>,
>;
#[derive(Clone, Debug)]
pub struct ClassifiedSdkError {
    pub classification: SdkErrorClassification,
    pub original: Value,
    pub suppress_turn_retry: bool,
    /// The selection refusal is context, not a replacement for the actual failure.
    pub pool_state: Option<Arc<AllAccountsBlockedError>>,
}
impl std::fmt::Display for ClassifiedSdkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.suppress_turn_retry {
            formatter.write_str(TURN_RETRY_SUPPRESSION_PREFIX)?;
        }
        if let Some(text) = self
            .original
            .as_str()
            .or_else(|| self.original["message"].as_str())
        {
            formatter.write_str(text)
        } else {
            write!(formatter, "{}", self.original)
        }
    }
}
impl std::error::Error for ClassifiedSdkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.pool_state.as_deref().map(|pool| pool as _)
    }
}
pub struct FailoverEvent {
    pub account: AccountSlot,
    pub next_account: Option<AccountSlot>,
    pub classification: SdkErrorClassification,
    pub attempt: usize,
    pub visible_delta_emitted: bool,
}
/// Waits before a transient same-account retry; injectable for tests.
pub type RetrySleep =
    dyn Fn(u64) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Sync;
pub struct FailoverOptions<'a> {
    pub accounts: &'a [AccountSlot],
    pub store: &'a dyn CredentialStore,
    pub provider: &'a str,
    pub now: &'a dyn Fn() -> f64,
    pub base_block_ms: f64,
    /// When `Some(false)`, only the selected account may be attempted; same-account retries remain enabled.
    pub account_fallback: Option<bool>,
    /// The requested model; a limit naming its family blocks only that family on the account.
    pub model: Option<&'a str>,
    /// The session this turn belongs to: blocks that bind only this session are remembered under it.
    pub session_key: Option<&'a str>,
    pub signal: Option<&'a maho_ai::utils::abort::AbortSignal>,
    pub sleep: Option<&'a RetrySleep>,
    /// Connection probes report the outcome without saving or clearing blocks.
    pub persist_blocks: bool,
    pub agent_dir: Option<&'a Path>,
    pub rechecked_accounts: Option<crate::limit_recheck::RecheckedAccounts>,
}
pub trait FailoverBoundary {
    fn select(&mut self, accounts: &[AccountSlot]) -> anyhow::Result<AccountSlot>;
    fn run_attempt(&mut self, account: AccountSlot) -> AttemptFactoryFuture<'_>;
    fn emit(&mut self, event: Value);
    fn classify(&self, error: &Value) -> SdkErrorClassification {
        crate::errors::classify_sdk_error(error)
    }
    /// Records a refused 1M window so the retry requests the standard one.
    fn on_long_context_refused(&mut self) {}
    fn error_from_event(&self, event: &Value) -> Option<Value> {
        if event["type"] == "error" {
            event.get("error").cloned()
        } else {
            None
        }
    }
    fn is_visible_delta(&self, event: &Value) -> bool {
        event["type"].as_str().is_some_and(|kind| {
            ["text", "thinking", "toolcall"].iter().any(|prefix| {
                ["start", "delta", "end"]
                    .iter()
                    .any(|suffix| kind == format!("{prefix}_{suffix}"))
            })
        })
    }
    fn on_failover(
        &mut self,
        event: FailoverEvent,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + '_>>;
}

#[derive(Clone, Debug, Default, PartialEq)]
struct BlockFields {
    blocked_until: Option<f64>,
    block_reason: Option<String>,
    blocked_at: Option<f64>,
    model_blocks: Option<ModelBlocks>,
}

/// Blocks that bind only the session that hit them, in memory for the process: a plain rate limit and a one-model
/// limit. Another session on the same account keeps using it and finds out on its own request (senpi#3466).
static SESSION_BLOCKS: LazyLock<Mutex<HashMap<String, HashMap<String, BlockFields>>>> =
    LazyLock::new(Default::default);

fn session_blocks() -> std::sync::MutexGuard<'static, HashMap<String, HashMap<String, BlockFields>>>
{
    SESSION_BLOCKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

pub fn error_text(error: &Value) -> String {
    error
        .as_str()
        .or_else(|| error["message"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| error.to_string())
}

/// Only a verdict that the account itself cannot serve anyone is shared through the credential store: a confirmed
/// account-level usage limit, a billing or organization refusal, or a rejected token (senpi#3466).
fn is_shared_verdict(classification: SdkErrorClassification, error: &Value) -> bool {
    match classification.kind {
        SdkErrorKind::AuthError | SdkErrorKind::Billing | SdkErrorKind::OrgNotAllowed => true,
        SdkErrorKind::RateLimit => {
            crate::errors::sdk_error_model_family(error).is_none()
                && crate::usage_limit_block::is_confirmed_account_usage_limit(&error_text(error))
        }
        _ => false,
    }
}

fn remember_session_block(session_key: Option<&str>, account: &AccountSlot) {
    let Some(session_key) = session_key else {
        return;
    };
    session_blocks()
        .entry(session_key.to_owned())
        .or_default()
        .insert(
            account.name.clone(),
            BlockFields {
                blocked_until: account.blocked_until,
                block_reason: account.block_reason.clone(),
                blocked_at: account.blocked_at,
                model_blocks: account.model_blocks.clone(),
            },
        );
}

/// Drops elapsed session blocks for every session, so sessions that ended leave nothing behind.
fn prune_session_blocks(now: f64) {
    let mut sessions = session_blocks();
    for blocks in sessions.values_mut() {
        blocks.retain(|_, block| {
            block.blocked_until.unwrap_or(0.0) > now
                || prune_model_blocks(block.model_blocks.as_ref(), now, None).is_some()
        });
    }
    sessions.retain(|_, blocks| !blocks.is_empty());
}

fn with_session_blocks(
    accounts: &[AccountSlot],
    session_key: Option<&str>,
    now: f64,
) -> Vec<AccountSlot> {
    prune_session_blocks(now);
    let sessions = session_blocks();
    let Some(blocks) = session_key.and_then(|key| sessions.get(key)) else {
        return accounts.to_vec();
    };
    accounts
        .iter()
        .map(|account| {
            let Some(own) = blocks.get(&account.name) else {
                return account.clone();
            };
            let mut merged = account.clone();
            if own.blocked_until.unwrap_or(0.0) > account.blocked_until.unwrap_or(0.0) {
                merged.blocked_until = own.blocked_until;
                merged.block_reason = own.block_reason.clone();
                merged.blocked_at = own.blocked_at;
            }
            merged.model_blocks = merge_model_blocks(
                account.model_blocks.as_ref(),
                own.model_blocks.as_ref(),
                0.0,
            );
            merged
        })
        .collect()
}

fn forget_session_block(session_key: Option<&str>, name: &str) {
    let Some(session_key) = session_key else {
        return;
    };
    let mut sessions = session_blocks();
    if let Some(blocks) = sessions.get_mut(session_key)
        && blocks.remove(name).is_some()
        && blocks.is_empty()
    {
        sessions.remove(session_key);
    }
}

/// `/claude-account unblock` lifts the account's session-scoped blocks in every session too. Returns whether one was
/// lifted.
pub fn forget_session_blocks_for(name: &str) -> bool {
    let mut sessions = session_blocks();
    let mut lifted = false;
    for blocks in sessions.values_mut() {
        lifted |= blocks.remove(name).is_some();
    }
    sessions.retain(|_, blocks| !blocks.is_empty());
    lifted
}

pub fn retry_after_ms(error: &Value) -> Option<f64> {
    if let Some(explicit) = error["retryAfterMs"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
    {
        return Some(explicit.ceil());
    }
    let text = error_text(error);
    static PATTERNS: std::sync::LazyLock<[(Option<regex::Regex>, f64); 2]> =
        std::sync::LazyLock::new(|| {
            [
                (
                    regex::Regex::new(r"(?i)\bretry[-_ ]?after[-_ ]?ms\s*[:=]\s*(\d+(?:\.\d+)?)")
                        .ok(),
                    1.0,
                ),
                (
                    regex::Regex::new(r"(?i)\bretry[-_ ]?after\s*[:=]\s*(\d+(?:\.\d+)?)").ok(),
                    1000.0,
                ),
            ]
        });
    for (regex, multiplier) in PATTERNS.iter() {
        if let Some(value) = regex.as_ref().and_then(|regex| {
            regex
                .captures(&text)
                .and_then(|capture| capture[1].parse::<f64>().ok())
        }) {
            return Some((value * multiplier).ceil());
        }
    }
    None
}
fn kind_name(kind: SdkErrorKind) -> &'static str {
    match kind {
        SdkErrorKind::RateLimit => "rate_limit",
        SdkErrorKind::Overloaded => "overloaded",
        SdkErrorKind::AuthError => "auth_error",
        SdkErrorKind::LoginUnreadable => "login_unreadable",
        SdkErrorKind::Billing => "billing",
        SdkErrorKind::OrgNotAllowed => "org_not_allowed",
        SdkErrorKind::Entitlement => "entitlement",
        SdkErrorKind::LongContextUnavailable => "long_context_unavailable",
        SdkErrorKind::Other => "other",
    }
}
pub fn blocked_account(
    account: &AccountSlot,
    classification: SdkErrorClassification,
    now: f64,
    attempt: i32,
    base_block_ms: f64,
    error: &Value,
    model: Option<&str>,
) -> AccountSlot {
    let mut blocked = account.clone();
    if classification.kind == SdkErrorKind::AuthError {
        // An auth block lasts until its re-probe time, not until re-login (senpi#3264).
        blocked.blocked_until = Some(
            now + maho_core::subscription_limits::usage_accounts::AUTH_BLOCK_REPROBE_MS as f64,
        );
        blocked.block_reason = Some("auth_error".into());
        blocked.blocked_at = Some(now);
        if blocked.is_claude_cli() {
            // Only a claude CLI login can be marked for a new sign-in; an env token account has no login to expire
            // (senpi#5350).
            if let Some(reason) = crate::errors::sign_in_required(error) {
                blocked.sign_in_required = Some(reason);
            }
            // The verdict names the sign-in it is about, so a block another engine writes is told apart (senpi#5623).
            blocked.auth_verdict = Some(crate::accounts::ClaudeCliAuthVerdict {
                dir_id: blocked.dir_id.clone().unwrap_or_default(),
                at: now,
            });
        }
        return blocked;
    }
    let fallback = (base_block_ms * 2_f64.powi(attempt)).min(MAX_RATE_LIMIT_BLOCK_MS);
    let text = error_text(error);
    if classification.kind == SdkErrorKind::RateLimit
        && let (Some(family), Some(model)) = (crate::errors::sdk_error_model_family(error), model)
    {
        // A model-scoped limit lasts until its own reset ("resets 8pm"); the account keeps serving other models.
        let reset = retry_after_ms(error).or_else(|| {
            maho_core::credential_pool::reset_time::usage_limit_reset_ms(&text, now as i64)
                .map(|ms| ms as f64)
        });
        let duration = reset
            .filter(|ms| *ms > 0.0)
            .unwrap_or(fallback)
            .min(MAX_RATE_LIMIT_BLOCK_MS);
        blocked.model_blocks = Some(with_model_block(
            account.model_blocks.as_ref(),
            &model_block_key(&family, model),
            now + duration,
            now,
        ));
        return blocked;
    }
    // A spent account is out until its limit resets, capped at the limit's own window (senpi#3462).
    let hinted = retry_after_ms(error).or_else(|| {
        crate::usage_limit_block::usage_limit_block_ms(&text, &account.name, now as i64)
            .map(|ms| ms as f64)
    });
    blocked.blocked_until = Some(now + hinted.unwrap_or(fallback).min(MAX_RATE_LIMIT_BLOCK_MS));
    blocked.block_reason = Some(kind_name(classification.kind).into());
    blocked.blocked_at = Some(now);
    blocked
}

fn block_state(
    account: &AccountSlot,
    stored: Option<&Value>,
    now: f64,
) -> anyhow::Result<Map<String, Value>> {
    let mut state = Map::new();
    if let Some(until) = account.blocked_until {
        state.insert("blockedUntil".into(), json!(until));
    }
    if let Some(reason) = &account.block_reason {
        state.insert("blockReason".into(), json!(reason));
    }
    if let Some(at) = account.blocked_at {
        state.insert("blockedAt".into(), json!(at));
    }
    if account.is_claude_cli() {
        if let Some(reason) = account.sign_in_required {
            state.insert("signInRequired".into(), serde_json::to_value(reason)?);
        }
        // Only this write's own auth verdict stays with the block (senpi#5623).
        if let Some(verdict) = &account.auth_verdict {
            state.insert("authVerdict".into(), serde_json::to_value(verdict)?);
        }
    }
    // Union with what a concurrent request stored meanwhile, later expiry winning.
    let existing: Option<ModelBlocks> = stored
        .and_then(|stored| stored.get("modelBlocks"))
        .and_then(|blocks| serde_json::from_value(blocks.clone()).ok());
    if let Some(merged) = merge_model_blocks(existing.as_ref(), account.model_blocks.as_ref(), now)
    {
        state.insert("modelBlocks".into(), serde_json::to_value(merged)?);
    }
    Ok(state)
}

#[derive(Default)]
enum BlockWrite {
    #[default]
    Unchanged,
    Saved,
    Superseded(Box<AccountSlot>),
}

/// An auth verdict can only be saved against the sign-in that actually failed.
async fn persist_block(
    store: &dyn CredentialStore,
    provider: &str,
    account: AccountSlot,
    now: f64,
) -> anyhow::Result<BlockWrite> {
    let result = Arc::new(Mutex::new(BlockWrite::Unchanged));
    let written = result.clone();
    store
        .modify(
            provider,
            Box::new(move |current| {
                Box::pin(async move {
                    let Some(Credential::OAuth(mut credential)) = current else {
                        return Ok(current);
                    };
                    if account.source == AccountSource::Env {
                        let states = credential
                            .extra
                            .entry("slotState")
                            .or_insert_with(|| json!({}));
                        let state = block_state(&account, states.get(&account.name), now)?;
                        states[&account.name] = Value::Object(state);
                        *written.lock().unwrap_or_else(PoisonError::into_inner) = BlockWrite::Saved;
                        return Ok(Some(Credential::OAuth(credential)));
                    }
                    let stored = credential
                        .extra
                        .get("accounts")
                        .and_then(Value::as_array)
                        .and_then(|accounts| {
                            accounts
                                .iter()
                                .find(|existing| existing["name"] == account.name)
                        })
                        .cloned();
                    // An auth_error is a verdict on the sign-in that failed: the token a token slot sent, or the account dir
                    // a claude CLI slot ran in. When a re-login replaced it meanwhile, nothing is written.
                    let superseded_material = |stored: &Value| {
                        if account.is_claude_cli() {
                            stored.get("dirId").and_then(Value::as_str) != account.dir_id.as_deref()
                        } else {
                            stored["access"] != account.access
                                || stored["refresh"] != account.refresh
                        }
                    };
                    if account.block_reason.as_deref() == Some("auth_error")
                        && let Some(stored) = &stored
                        && superseded_material(stored)
                    {
                        *written.lock().unwrap_or_else(PoisonError::into_inner) =
                            BlockWrite::Superseded(Box::new(serde_json::from_value(
                                stored.clone(),
                            )?));
                        return Ok(Some(Credential::OAuth(credential)));
                    }
                    if let Some(accounts) = credential
                        .extra
                        .get_mut("accounts")
                        .and_then(Value::as_array_mut)
                    {
                        for existing in accounts {
                            if existing["name"] != account.name {
                                continue;
                            }
                            let state = block_state(&account, Some(&*existing), now)?;
                            let object = existing
                                .as_object_mut()
                                .ok_or_else(|| anyhow::anyhow!("account must be object"))?;
                            for key in [
                                "blockedUntil",
                                "blockReason",
                                "blockedAt",
                                "modelBlocks",
                                "authVerdict",
                            ] {
                                object.remove(key);
                            }
                            object.extend(state);
                            *written.lock().unwrap_or_else(PoisonError::into_inner) =
                                BlockWrite::Saved;
                        }
                    }
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await?;
    Ok(std::mem::take(
        &mut *result.lock().unwrap_or_else(PoisonError::into_inner),
    ))
}

fn clearable(slot: &Value, started_at: f64) -> bool {
    slot["blockedUntil"].is_number()
        && slot["blockReason"] != "auth_error"
        && slot["blockedAt"].as_f64().unwrap_or(0.0) <= started_at
}

/// An account that just served a turn is not spent: a limit block stored before the turn started is cleared. A block
/// stored after the turn started is newer evidence and stays; auth blocks keep their re-probe.
pub async fn clear_block_after_success(
    store: &dyn CredentialStore,
    provider: &str,
    name: &str,
    started_at: f64,
) -> anyhow::Result<()> {
    let Some(Credential::OAuth(read)) = store.read(provider, None).await? else {
        return Ok(());
    };
    let slot = read
        .extra
        .get("accounts")
        .and_then(Value::as_array)
        .and_then(|accounts| accounts.iter().find(|account| account["name"] == name))
        .or_else(|| {
            read.extra
                .get("slotState")
                .and_then(|states| states.get(name))
        });
    if !slot.is_some_and(|slot| clearable(slot, started_at)) {
        return Ok(());
    }
    let name = name.to_owned();
    store
        .modify(
            provider,
            Box::new(move |current| {
                Box::pin(async move {
                    let Some(Credential::OAuth(mut credential)) = current else {
                        return Ok(current);
                    };
                    if let Some(state) = credential
                        .extra
                        .get_mut("slotState")
                        .and_then(|states| states.get_mut(&name))
                        && clearable(state, started_at)
                    {
                        *state = match state.get("modelBlocks").cloned() {
                            Some(blocks) => json!({ "modelBlocks": blocks }),
                            None => json!({}),
                        };
                    }
                    if let Some(accounts) = credential
                        .extra
                        .get_mut("accounts")
                        .and_then(Value::as_array_mut)
                    {
                        for account in accounts {
                            if account["name"] == name.as_str()
                                && clearable(account, started_at)
                                && let Some(object) = account.as_object_mut()
                            {
                                for key in ["blockedUntil", "blockReason", "blockedAt"] {
                                    object.remove(key);
                                }
                            }
                        }
                    }
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await?;
    Ok(())
}

fn usable(account: &AccountSlot, now: f64, model: Option<&str>) -> bool {
    account.block_reason.is_none()
        && account.blocked_until.is_none_or(|until| until <= now)
        && crate::model_blocks::active_model_block_until(account.model_blocks.as_ref(), model, now)
            .is_none()
}

fn is_transient(classification: SdkErrorClassification) -> bool {
    matches!(
        classification.kind,
        SdkErrorKind::Overloaded | SdkErrorKind::Other
    )
}

/// What the caller does after a failed attempt.
#[derive(Debug)]
pub enum FailureStep {
    /// Retry the same account, after `delay_ms` when set; `long_context_refused` asks the caller to record the
    /// refused 1M window before the retry builds its options (senpi#3408).
    RetrySame {
        account: String,
        delay_ms: Option<u64>,
        long_context_refused: bool,
    },
    /// The rejected token was already replaced in the store: select again on the stored material.
    Reselect,
    /// The account is blocked; another account may serve. The caller selects it and reports the failover.
    Rotate {
        blocked: AccountSlot,
        classification: SdkErrorClassification,
    },
    /// The account is blocked and no other attempt may run: report the failover, then fail with `error`.
    Stop {
        blocked: AccountSlot,
        classification: SdkErrorClassification,
        error: ClassifiedSdkError,
    },
}

pub struct FailoverTurnOptions<'a> {
    pub store: &'a dyn CredentialStore,
    pub provider: &'a str,
    pub model: Option<&'a str>,
    pub session_key: Option<&'a str>,
    pub account_fallback: Option<bool>,
    pub base_block_ms: f64,
    pub now: f64,
    pub persist_blocks: bool,
    pub agent_dir: Option<&'a Path>,
    pub rechecked_accounts: Option<crate::limit_recheck::RecheckedAccounts>,
}

/// `runFailover`'s per-turn decisions, shared by [`run_failover`] and the native stream loop: one attempt per
/// account for account-level failures, at most [`TRANSIENT_RETRIES_PER_TURN`] same-account retries for a transient
/// failure (it never rotates), one retry at the standard window for a refused 1M window, and one retry on newer
/// stored credentials.
pub struct FailoverTurn<'a> {
    pub accounts: Vec<AccountSlot>,
    store: &'a dyn CredentialStore,
    provider: &'a str,
    model: Option<&'a str>,
    session_key: Option<&'a str>,
    base_block_ms: f64,
    attempt_limit: usize,
    attempt: usize,
    transient_retries_used: u32,
    long_context_retried: bool,
    retried_on_stored_material: HashSet<String>,
    last_error: Option<ClassifiedSdkError>,
    persist_blocks: bool,
    agent_dir: Option<&'a Path>,
    rechecked_accounts: Option<crate::limit_recheck::RecheckedAccounts>,
    rechecking: bool,
}

impl<'a> FailoverTurn<'a> {
    pub fn new(accounts: &[AccountSlot], options: FailoverTurnOptions<'a>) -> Self {
        let accounts = crate::affinity::clear_expired_blocks(
            &with_session_blocks(accounts, options.session_key, options.now),
            options.now,
        );
        let attempt_limit = if options.account_fallback == Some(false) {
            accounts.len().min(1)
        } else {
            accounts.len()
        };
        Self {
            accounts,
            store: options.store,
            provider: options.provider,
            model: options.model,
            session_key: options.session_key,
            base_block_ms: options.base_block_ms,
            attempt_limit,
            attempt: 0,
            transient_retries_used: 0,
            long_context_retried: false,
            retried_on_stored_material: HashSet::new(),
            last_error: None,
            persist_blocks: options.persist_blocks,
            agent_dir: options.agent_dir,
            rechecked_accounts: options.rechecked_accounts,
            rechecking: false,
        }
    }
    pub fn attempts_left(&self) -> bool {
        self.attempt < self.attempt_limit
    }
    /// Account attempts that failed with an account-level verdict so far.
    pub fn attempt(&self) -> usize {
        self.attempt
    }
    pub fn account(&self, name: &str) -> Option<AccountSlot> {
        self.accounts.iter().find(|slot| slot.name == name).cloned()
    }
    /// The failure the turn ends with when no attempt is left.
    pub fn last_error(&self) -> anyhow::Error {
        self.last_error
            .clone()
            .map(anyhow::Error::from)
            .unwrap_or_else(|| {
                anyhow::anyhow!("Anthropic Subscription failover exhausted without an attempt")
            })
    }
    pub(crate) fn selection_error(&self, error: anyhow::Error) -> anyhow::Error {
        match (
            self.last_error.clone(),
            error.downcast::<AllAccountsBlockedError>(),
        ) {
            (Some(mut original), Ok(pool)) => {
                original.pool_state = Some(Arc::new(pool));
                original.into()
            }
            (None, Ok(pool)) => pool.into(),
            (_, Err(error)) => error,
        }
    }
    /// Only a pool refusal opens the half-open selection view. Claim selection and record the
    /// attempt under one lock, so concurrent requests cannot spend the same allowance twice.
    pub(crate) fn select(
        &mut self,
        now: f64,
        mut select: impl FnMut(&[AccountSlot]) -> anyhow::Result<AccountSlot>,
    ) -> anyhow::Result<AccountSlot> {
        self.rechecking = false;
        let ordinary = select(&self.accounts);
        let Some(attempted) = &self.rechecked_accounts else {
            return ordinary;
        };
        let mut attempted = attempted.lock().unwrap_or_else(PoisonError::into_inner);
        let account = match ordinary {
            Err(error) if error.is::<AllAccountsBlockedError>() => {
                let view = crate::limit_recheck::recheck_selection(&self.accounts, &attempted, now);
                match select(&view) {
                    Ok(account) => {
                        self.rechecking = true;
                        account
                    }
                    Err(refusal) if refusal.is::<AllAccountsBlockedError>() => return Err(error),
                    Err(error) => return Err(error),
                }
            }
            other => other?,
        };
        attempted.insert(account.name.clone());
        Ok(account)
    }
    fn replace(&mut self, replacement: AccountSlot) {
        for account in &mut self.accounts {
            if account.name == replacement.name {
                *account = replacement.clone();
            }
        }
    }
    /// Classifies a failed attempt; `Err` ends the turn with that error.
    pub async fn on_failure(
        &mut self,
        account: &AccountSlot,
        failure: Value,
        classification: SdkErrorClassification,
        visible: bool,
        now: f64,
    ) -> anyhow::Result<FailureStep> {
        let classified = ClassifiedSdkError {
            classification,
            original: failure.clone(),
            suppress_turn_retry: visible,
            pool_state: None,
        };
        self.last_error = Some(classified.clone());
        if !classification.retryable {
            return Err(classified.into());
        }
        if self.rechecking
            && (is_transient(classification)
                || classification.kind == SdkErrorKind::LongContextUnavailable)
        {
            // An inconclusive half-open attempt leaves the saved block intact.
            return Err(classified.into());
        }
        // A refused 1M window is the account's entitlement, not a limit: never block or rotate it (senpi#3408).
        if classification.kind == SdkErrorKind::LongContextUnavailable {
            if visible || self.long_context_retried {
                return Err(classified.into());
            }
            self.long_context_retried = true;
            return Ok(FailureStep::RetrySame {
                account: account.name.clone(),
                delay_ms: None,
                long_context_refused: true,
            });
        }
        // A login this process could not read (senpi#5630) moves this turn to another account and is stored nowhere:
        // other sessions may read it fine, and an unlocked keychain must work on the very next turn.
        if classification.kind == SdkErrorKind::LoginUnreadable {
            if visible || self.attempt + 1 >= self.attempt_limit {
                return Err(classified.into());
            }
            let mut unreadable = account.clone();
            unreadable.blocked_until = Some(
                now + maho_core::subscription_limits::usage_accounts::AUTH_BLOCK_REPROBE_MS as f64,
            );
            unreadable.block_reason = Some("login_unreadable".into());
            unreadable.blocked_at = Some(now);
            self.replace(unreadable.clone());
            self.attempt += 1;
            return Ok(FailureStep::Rotate {
                blocked: unreadable,
                classification,
            });
        }
        // A transient failure says nothing about the account: retried in place a bounded number of times, never
        // blocked or rotated (senpi#2891, senpi#3466).
        if is_transient(classification) {
            if visible || self.transient_retries_used >= TRANSIENT_RETRIES_PER_TURN {
                return Err(classified.into());
            }
            let delay_ms = TRANSIENT_RETRY_DELAY_MS * 2_u64.pow(self.transient_retries_used);
            self.transient_retries_used += 1;
            return Ok(FailureStep::RetrySame {
                account: account.name.clone(),
                delay_ms: Some(delay_ms),
                long_context_refused: false,
            });
        }
        let blocked = blocked_account(
            account,
            classification,
            now,
            self.attempt as i32,
            self.base_block_ms,
            &failure,
            self.model,
        );
        // With no session to hold it, a block is shared as before.
        let shared = is_shared_verdict(classification, &failure) || self.session_key.is_none();
        let write = if !self.persist_blocks {
            BlockWrite::Unchanged
        } else if shared {
            persist_block(self.store, self.provider, blocked.clone(), now).await?
        } else {
            remember_session_block(self.session_key, &blocked);
            BlockWrite::Saved
        };
        if let BlockWrite::Superseded(superseded) = &write
            && !self.rechecking
            && !visible
            && usable(superseded, now, self.model)
            && self.retried_on_stored_material.insert(account.name.clone())
        {
            self.replace(superseded.as_ref().clone());
            return Ok(FailureStep::Reselect);
        }
        if matches!(write, BlockWrite::Saved)
            && let Some(agent_dir) = self.agent_dir
        {
            let until = blocked.blocked_until.or_else(|| {
                crate::model_blocks::active_model_block_until(
                    blocked.model_blocks.as_ref(),
                    self.model,
                    now,
                )
            });
            crate::account_block_log::log_account_blocked(agent_dir, &blocked, until, shared);
        }
        self.replace(blocked.clone());
        self.attempt += 1;
        if visible || !self.attempts_left() {
            return Ok(FailureStep::Stop {
                blocked,
                classification,
                error: classified,
            });
        }
        Ok(FailureStep::Rotate {
            blocked,
            classification,
        })
    }
    /// A finished turn: the account's session block is dropped and a stale stored limit block cleared. Clearing is
    /// best-effort hygiene, so a store failure is logged and never fails the turn.
    pub async fn on_success(&self, name: &str, started_at: f64) {
        if !self.persist_blocks {
            return;
        }
        forget_session_block(self.session_key, name);
        if let Err(error) =
            clear_block_after_success(self.store, self.provider, name, started_at).await
        {
            maho_ext_api::diagnostic(&format!(
                "senpi anthropic-subscription: could not clear a stored block on {name}: {error}"
            ));
        }
    }
}

/// Sleeps before a transient retry; an abort ends the wait with `false`.
pub async fn retry_delay(
    delay_ms: u64,
    signal: Option<&maho_ai::utils::abort::AbortSignal>,
    sleep: Option<&RetrySleep>,
) -> bool {
    let wait = async {
        match sleep {
            Some(sleep) => sleep(delay_ms).await,
            None => tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await,
        }
    };
    match signal {
        Some(signal) => maho_ai::utils::abort::race_with_abort_signal(wait, signal)
            .await
            .is_ok(),
        None => {
            wait.await;
            true
        }
    }
}

/// Runs the attempts of one turn. A retry is transparent only before a text, thinking, or tool-call event reaches the
/// caller; post-delta failures are marked so AgentSession never replays the partial turn.
pub async fn run_failover(
    options: FailoverOptions<'_>,
    boundary: &mut impl FailoverBoundary,
) -> anyhow::Result<()> {
    let mut turn = FailoverTurn::new(
        options.accounts,
        FailoverTurnOptions {
            store: options.store,
            provider: options.provider,
            model: options.model,
            session_key: options.session_key,
            account_fallback: options.account_fallback,
            base_block_ms: options.base_block_ms,
            now: (options.now)(),
            persist_blocks: options.persist_blocks,
            agent_dir: options.agent_dir,
            rechecked_accounts: options.rechecked_accounts,
        },
    );
    let mut retry: Option<AccountSlot> = None;
    while turn.attempts_left() {
        // Each attempt gets its own copy, so a concurrent attempt still reports the token it actually sent.
        let account = match retry.take() {
            Some(account) => account,
            None => turn.select((options.now)(), |accounts| boundary.select(accounts))?,
        };
        let started_at = (options.now)();
        let mut visible = false;
        let failure = match boundary.run_attempt(account.clone()).await {
            Err(error) => error,
            Ok(mut messages) => loop {
                match messages.next().await {
                    Ok(None) => {
                        turn.on_success(&account.name, started_at).await;
                        return Ok(());
                    }
                    Err(error) => break error,
                    Ok(Some(event)) => {
                        if let Some(error) = boundary.error_from_event(&event) {
                            break error;
                        }
                        visible |= boundary.is_visible_delta(&event);
                        boundary.emit(event);
                    }
                }
            },
        };
        let classification = boundary.classify(&failure);
        match turn
            .on_failure(&account, failure, classification, visible, (options.now)())
            .await?
        {
            FailureStep::RetrySame {
                account,
                delay_ms,
                long_context_refused,
            } => {
                if long_context_refused {
                    boundary.on_long_context_refused();
                }
                if let Some(delay_ms) = delay_ms
                    && !retry_delay(delay_ms, options.signal, options.sleep).await
                {
                    return Err(turn.last_error());
                }
                retry = turn.account(&account);
            }
            FailureStep::Reselect => {}
            FailureStep::Rotate {
                blocked,
                classification,
            } => {
                let selection = turn.select((options.now)(), |accounts| boundary.select(accounts));
                boundary
                    .on_failover(FailoverEvent {
                        account: blocked,
                        next_account: selection.as_ref().ok().cloned(),
                        classification,
                        attempt: turn.attempt(),
                        visible_delta_emitted: false,
                    })
                    .await?;
                retry = Some(selection.map_err(|error| turn.selection_error(error))?);
            }
            FailureStep::Stop {
                blocked,
                classification,
                error,
            } => {
                boundary
                    .on_failover(FailoverEvent {
                        account: blocked,
                        next_account: None,
                        classification,
                        attempt: turn.attempt(),
                        visible_delta_emitted: error.suppress_turn_retry,
                    })
                    .await?;
                if !error.suppress_turn_retry
                    && let Err(refusal) = boundary.select(&turn.accounts)
                    && refusal.is::<AllAccountsBlockedError>()
                {
                    return Err(turn.selection_error(refusal));
                }
                return Err(error.into());
            }
        }
    }
    Err(turn.last_error())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Messages(std::collections::VecDeque<Result<Value, Value>>);
    impl AttemptMessages for Messages {
        fn next(&mut self) -> AttemptFuture<'_> {
            Box::pin(async { self.0.pop_front().transpose() })
        }
    }
    struct Boundary {
        streams: std::collections::VecDeque<Vec<Result<Value, Value>>>,
        emitted: Vec<Value>,
        attempts: Vec<String>,
        failovers: Vec<FailoverEvent>,
    }
    impl FailoverBoundary for Boundary {
        fn select(&mut self, accounts: &[AccountSlot]) -> anyhow::Result<AccountSlot> {
            Ok(crate::affinity::select_account(
                accounts,
                &crate::affinity::AffinityOptions {
                    pinned_account: Some("a"),
                    ..Default::default()
                },
                0.0,
            )?)
        }
        fn run_attempt(&mut self, account: AccountSlot) -> AttemptFactoryFuture<'_> {
            self.attempts.push(account.name);
            let messages = self.streams.pop_front().expect("attempt stream");
            Box::pin(
                async move { Ok(Box::new(Messages(messages.into())) as Box<dyn AttemptMessages>) },
            )
        }
        fn emit(&mut self, event: Value) {
            self.emitted.push(event);
        }
        fn on_failover(
            &mut self,
            event: FailoverEvent,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + '_>>
        {
            self.failovers.push(event);
            Box::pin(async { Ok(()) })
        }
    }
    #[tokio::test]
    async fn retries_only_before_visible_delta_and_persists_failed_account() {
        use maho_ai::auth::credential_store::InMemoryCredentialStore;
        for visible in [false, true] {
            let accounts: Vec<_> = ["a", "b"]
                .into_iter()
                .map(|name| AccountSlot {
                    name: name.into(),
                    display_name: None,
                    refresh: String::new(),
                    access: String::new(),
                    expires: 0.0,
                    source: AccountSource::Login,
                    blocked_until: None,
                    block_reason: None,
                    ..Default::default()
                })
                .collect();
            let store = InMemoryCredentialStore::new();
            let credential = crate::accounts::empty_credential()
                .with_extra("accounts", serde_json::json!(accounts));
            store
                .modify(
                    "provider",
                    Box::new(move |_| {
                        Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })
                    }),
                    None,
                )
                .await
                .expect("seed");
            let mut first = Vec::new();
            if visible {
                first.push(Ok(
                    serde_json::json!({"type":"text_delta","delta":"partial"}),
                ));
            }
            first.push(Err(serde_json::json!("rate_limit")));
            let mut boundary = Boundary {
                streams: vec![
                    first,
                    vec![Ok(
                        serde_json::json!({"type":"text_delta","delta":"success"}),
                    )],
                ]
                .into(),
                emitted: Vec::new(),
                attempts: Vec::new(),
                failovers: Vec::new(),
            };
            let result = run_failover(
                FailoverOptions {
                    accounts: &accounts,
                    store: &store,
                    provider: "provider",
                    now: &|| 0.0,
                    base_block_ms: DEFAULT_RATE_LIMIT_BLOCK_MS,
                    account_fallback: None,
                    model: None,
                    session_key: None,
                    signal: None,
                    sleep: None,
                    persist_blocks: true,
                    agent_dir: None,
                    rechecked_accounts: None,
                },
                &mut boundary,
            )
            .await;
            assert_eq!(boundary.attempts.len(), if visible { 1 } else { 2 });
            assert_eq!(boundary.failovers.len(), 1);
            assert_eq!(boundary.failovers[0].visible_delta_emitted, visible);
            if visible {
                assert!(
                    result
                        .expect_err("partial failure")
                        .downcast_ref::<ClassifiedSdkError>()
                        .expect("classified")
                        .suppress_turn_retry
                );
            } else {
                result.expect("failover success");
                assert_eq!(boundary.attempts, ["a", "b"]);
            }
            let credential = store
                .read("provider", None)
                .await
                .expect("read")
                .expect("credential")
                .into_oauth()
                .expect("oauth");
            assert_eq!(
                credential.extra["accounts"][0]["blockedUntil"],
                DEFAULT_RATE_LIMIT_BLOCK_MS
            );
        }
    }
    #[test]
    fn explicit_backoff_clamps_and_auth_blocks_until_reprobe() {
        let slot = AccountSlot {
            name: "env".into(),
            display_name: None,
            refresh: String::new(),
            access: String::new(),
            expires: 0.0,
            source: AccountSource::Env,
            blocked_until: Some(42.0),
            block_reason: None,
            ..Default::default()
        };
        let classification = SdkErrorClassification {
            kind: SdkErrorKind::RateLimit,
            retryable: true,
        };
        assert_eq!(
            retry_after_ms(&serde_json::json!("retry-after: 1.25")),
            Some(1250.0)
        );
        assert_eq!(
            retry_after_ms(&serde_json::json!({"retryAfterMs":2.1})),
            Some(3.0)
        );
        assert_eq!(
            blocked_account(
                &slot,
                classification,
                10.0,
                0,
                DEFAULT_RATE_LIMIT_BLOCK_MS,
                &serde_json::json!({"retryAfterMs":1e12}),
                None
            )
            .blocked_until,
            Some(10.0 + MAX_RATE_LIMIT_BLOCK_MS)
        );
        assert_eq!(
            blocked_account(
                &slot,
                SdkErrorClassification {
                    kind: SdkErrorKind::AuthError,
                    retryable: true
                },
                10.0,
                0,
                DEFAULT_RATE_LIMIT_BLOCK_MS,
                &Value::Null,
                None
            )
            .blocked_until,
            Some(
                10.0 + maho_core::subscription_limits::usage_accounts::AUTH_BLOCK_REPROBE_MS as f64
            )
        );
    }
    #[tokio::test]
    async fn env_block_persistence_does_not_copy_token_fields() {
        use crate::accounts::empty_credential;
        use maho_ai::auth::credential_store::InMemoryCredentialStore;
        let store = InMemoryCredentialStore::new();
        store
            .modify(
                "provider",
                Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(empty_credential()))) })),
                None,
            )
            .await
            .expect("seed");
        let account = AccountSlot {
            name: "env".into(),
            display_name: None,
            refresh: "not-stored".into(),
            access: "not-stored".into(),
            expires: 0.0,
            source: AccountSource::Env,
            blocked_until: Some(123.0),
            block_reason: Some("rate_limit".into()),
            ..Default::default()
        };
        persist_block(&store, "provider", account, 0.0)
            .await
            .expect("block");
        let credential = store
            .read("provider", None)
            .await
            .expect("read")
            .expect("credential")
            .into_oauth()
            .expect("oauth");
        assert_eq!(credential.extra["slotState"]["env"]["blockedUntil"], 123.0);
        assert!(credential.extra["slotState"]["env"].get("access").is_none());
    }

    fn slot(name: &str) -> AccountSlot {
        AccountSlot {
            name: name.into(),
            access: format!("access-{name}"),
            refresh: format!("refresh-{name}"),
            ..Default::default()
        }
    }
    async fn seeded(
        accounts: &[AccountSlot],
    ) -> maho_ai::auth::credential_store::InMemoryCredentialStore {
        let store = maho_ai::auth::credential_store::InMemoryCredentialStore::new();
        let credential =
            crate::accounts::empty_credential().with_extra("accounts", serde_json::json!(accounts));
        store
            .modify(
                "provider",
                Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .expect("seed");
        store
    }
    async fn stored(store: &maho_ai::auth::credential_store::InMemoryCredentialStore) -> Value {
        Value::Object(
            store
                .read("provider", None)
                .await
                .expect("read")
                .expect("credential")
                .into_oauth()
                .expect("oauth")
                .extra
                .into_iter()
                .collect(),
        )
    }
    fn turn<'a>(
        accounts: &'a [AccountSlot],
        store: &'a maho_ai::auth::credential_store::InMemoryCredentialStore,
        session_key: Option<&'a str>,
        model: Option<&'a str>,
    ) -> FailoverTurn<'a> {
        FailoverTurn::new(
            accounts,
            FailoverTurnOptions {
                store,
                provider: "provider",
                model,
                session_key,
                account_fallback: None,
                base_block_ms: DEFAULT_RATE_LIMIT_BLOCK_MS,
                now: 0.0,
                persist_blocks: true,
                agent_dir: None,
                rechecked_accounts: None,
            },
        )
    }
    fn classify(error: &Value) -> SdkErrorClassification {
        crate::errors::classify_sdk_error(error)
    }
    #[tokio::test]
    async fn transient_failures_retry_in_place_twice_then_fail_without_a_block() {
        let accounts = [slot("a"), slot("b")];
        let store = seeded(&accounts).await;
        let mut turn = turn(&accounts, &store, Some("session-transient"), None);
        let error = serde_json::json!("overloaded_error: busy");
        let mut delays = Vec::new();
        for _ in 0..TRANSIENT_RETRIES_PER_TURN {
            match turn
                .on_failure(&accounts[0], error.clone(), classify(&error), false, 0.0)
                .await
                .expect("retry")
            {
                FailureStep::RetrySame {
                    account, delay_ms, ..
                } => {
                    assert_eq!(account, "a");
                    delays.push(delay_ms.expect("backoff"));
                }
                other => panic!("expected a same-account retry, got {other:?}"),
            }
        }
        assert_eq!(
            delays,
            [TRANSIENT_RETRY_DELAY_MS, 2 * TRANSIENT_RETRY_DELAY_MS]
        );
        assert!(
            turn.on_failure(&accounts[0], error.clone(), classify(&error), false, 0.0)
                .await
                .is_err()
        );
        assert!(
            stored(&store).await["accounts"][0]
                .get("blockedUntil")
                .is_none()
        );
    }
    #[tokio::test]
    async fn plain_rate_limits_bind_only_the_session_and_success_lifts_them() {
        let accounts = [slot("a"), slot("b")];
        let store = seeded(&accounts).await;
        let error = serde_json::json!("rate_limit_error: slow down");
        let mut first = turn(&accounts, &store, Some("session-scoped"), None);
        assert!(matches!(
            first
                .on_failure(&accounts[0], error.clone(), classify(&error), false, 0.0)
                .await
                .expect("rotate"),
            FailureStep::Rotate { .. }
        ));
        assert!(
            stored(&store).await["accounts"][0]
                .get("blockedUntil")
                .is_none()
        );
        let again = turn(&accounts, &store, Some("session-scoped"), None);
        assert!(again.account("a").expect("a").blocked_until.is_some());
        let other = turn(&accounts, &store, Some("session-other"), None);
        assert!(other.account("a").expect("a").blocked_until.is_none());
        assert!(forget_session_blocks_for("a"));
        assert!(!forget_session_blocks_for("a"));
    }
    #[tokio::test]
    async fn confirmed_usage_limits_are_shared_and_cleared_after_a_later_success() {
        let accounts = [slot("a"), slot("b")];
        let store = seeded(&accounts).await;
        let error = serde_json::json!("You've hit your session limit · resets 3pm");
        let mut turn = turn(&accounts, &store, Some("session-shared"), None);
        turn.on_failure(&accounts[0], error.clone(), classify(&error), false, 10.0)
            .await
            .expect("rotate");
        let stored_slot = stored(&store).await["accounts"][0].clone();
        assert_eq!(stored_slot["blockReason"], "rate_limit");
        assert_eq!(stored_slot["blockedAt"], 10.0);
        clear_block_after_success(&store, "provider", "a", 5.0)
            .await
            .expect("older turn");
        assert!(stored(&store).await["accounts"][0]["blockedUntil"].is_number());
        clear_block_after_success(&store, "provider", "a", 10.0)
            .await
            .expect("newer turn");
        assert!(
            stored(&store).await["accounts"][0]
                .get("blockedUntil")
                .is_none()
        );
    }
    #[tokio::test]
    async fn a_model_family_limit_blocks_only_that_family() {
        let accounts = [slot("a"), slot("b")];
        let store = seeded(&accounts).await;
        let error = serde_json::json!("You've hit your Opus limit (rate_limit)");
        let mut turn = turn(&accounts, &store, None, Some("claude-opus-5"));
        let FailureStep::Rotate { blocked, .. } = turn
            .on_failure(&accounts[0], error.clone(), classify(&error), false, 0.0)
            .await
            .expect("rotate")
        else {
            panic!("model limit rotates");
        };
        assert_eq!(blocked.blocked_until, None);
        assert!(
            blocked
                .model_blocks
                .expect("model blocks")
                .contains_key("opus")
        );
        assert!(
            stored(&store).await["accounts"][0]["modelBlocks"]["opus"]["blockedUntil"].is_number()
        );
    }
    #[tokio::test]
    async fn an_auth_error_on_replaced_material_retries_the_stored_token_once() {
        let accounts = [slot("a"), slot("b")];
        let mut rotated = slot("a");
        rotated.access = "fresh".into();
        rotated.refresh = "fresh-refresh".into();
        let store = seeded(&[rotated, slot("b")]).await;
        let error = serde_json::json!("authentication_failed: token revoked");
        let mut turn = turn(&accounts, &store, None, None);
        assert!(matches!(
            turn.on_failure(&accounts[0], error.clone(), classify(&error), false, 0.0)
                .await
                .expect("reselect"),
            FailureStep::Reselect
        ));
        assert_eq!(turn.account("a").expect("a").access, "fresh");
        assert!(
            stored(&store).await["accounts"][0]
                .get("blockReason")
                .is_none()
        );
    }
    #[tokio::test]
    async fn account_fallback_off_allows_one_account_attempt() {
        let accounts = [slot("a"), slot("b")];
        let store = seeded(&accounts).await;
        let mut turn = FailoverTurn::new(
            &accounts,
            FailoverTurnOptions {
                store: &store,
                provider: "provider",
                model: None,
                session_key: None,
                account_fallback: Some(false),
                base_block_ms: DEFAULT_RATE_LIMIT_BLOCK_MS,
                now: 0.0,
                persist_blocks: true,
                agent_dir: None,
                rechecked_accounts: None,
            },
        );
        let error = serde_json::json!("rate_limit_error");
        assert!(matches!(
            turn.on_failure(&accounts[0], error.clone(), classify(&error), false, 0.0)
                .await
                .expect("stop"),
            FailureStep::Stop { .. }
        ));
    }

    #[tokio::test]
    async fn connection_probe_neither_writes_nor_clears_blocks() {
        let primary = slot("probe-primary");
        let mut spent = slot("probe-spent");
        spent.blocked_until = Some(100.0);
        spent.block_reason = Some("rate_limit".into());
        spent.blocked_at = Some(1.0);
        let accounts = [primary, spent];
        let store = seeded(&accounts).await;
        let directory = tempfile::tempdir().expect("probe log root");
        let mut probe = FailoverTurn::new(
            &accounts,
            FailoverTurnOptions {
                store: &store,
                provider: "provider",
                model: None,
                session_key: None,
                account_fallback: Some(false),
                base_block_ms: DEFAULT_RATE_LIMIT_BLOCK_MS,
                now: 2.0,
                persist_blocks: false,
                agent_dir: Some(directory.path()),
                rechecked_accounts: None,
            },
        );
        let failure = json!("authentication_failed: token revoked");
        assert!(matches!(
            probe
                .on_failure(
                    &accounts[0],
                    failure.clone(),
                    classify(&failure),
                    false,
                    2.0
                )
                .await
                .expect("probe verdict"),
            FailureStep::Stop { .. }
        ));
        probe.on_success(&accounts[1].name, 2.0).await;
        let stored = stored(&store).await;
        assert!(stored["accounts"][0].get("blockReason").is_none());
        assert_eq!(stored["accounts"][1]["blockedUntil"], 100.0);
        assert!(!directory.path().join("logs/session.log").exists());
    }

    #[tokio::test]
    async fn a_half_open_check_is_once_per_account_per_turn_and_keeps_an_inconclusive_block() {
        let mut account = slot("recheck-sole");
        account.block_reason = Some("rate_limit".into());
        account.blocked_until = Some(100.0);
        account.blocked_at = Some(0.0);
        let accounts = [account];
        let store = seeded(&accounts).await;
        let directory = tempfile::tempdir().expect("recheck root");
        let make_turn = |timestamp| maho_ai::types::Context {
            messages: vec![maho_ai::types::Message::User(maho_ai::types::UserMessage {
                content: maho_ai::types::UserContent::Text("check".into()),
                timestamp,
            })],
            ..Default::default()
        };
        for (timestamp, attempts) in [(1, 1), (1, 0), (2, 1)] {
            let mut boundary = Boundary {
                streams: [vec![Err(json!("overloaded_error: busy"))]].into(),
                emitted: vec![],
                attempts: vec![],
                failovers: vec![],
            };
            let rechecked_accounts = crate::limit_recheck::accounts_for_turn(
                directory.path(),
                Some("session"),
                &make_turn(timestamp),
            );
            let result = run_failover(
                FailoverOptions {
                    accounts: &accounts,
                    store: &store,
                    provider: "provider",
                    now: &|| 0.0,
                    base_block_ms: DEFAULT_RATE_LIMIT_BLOCK_MS,
                    account_fallback: None,
                    model: None,
                    session_key: None,
                    signal: None,
                    sleep: None,
                    persist_blocks: true,
                    agent_dir: None,
                    rechecked_accounts,
                },
                &mut boundary,
            )
            .await
            .expect_err("inconclusive or unavailable");
            assert_eq!(boundary.attempts.len(), attempts);
            if attempts == 0 {
                assert!(result.is::<AllAccountsBlockedError>());
            } else {
                assert!(result.is::<ClassifiedSdkError>());
            }
            assert_eq!(stored(&store).await["accounts"][0]["blockedUntil"], 100.0);
        }
    }

    #[tokio::test]
    async fn a_refused_pool_is_context_for_the_original_sdk_failure() {
        let mut spent = slot("spent");
        spent.block_reason = Some("rate_limit".into());
        spent.blocked_until = Some(3000.0);
        let accounts = [slot("a"), spent];
        let store = seeded(&accounts).await;
        let mut boundary = Boundary {
            streams: [vec![Err(json!("authentication_failed: token revoked"))]].into(),
            attempts: vec![],
            emitted: vec![],
            failovers: vec![],
        };
        let result = run_failover(
            FailoverOptions {
                accounts: &accounts,
                store: &store,
                provider: "provider",
                now: &|| 0.0,
                base_block_ms: DEFAULT_RATE_LIMIT_BLOCK_MS,
                account_fallback: None,
                model: None,
                session_key: None,
                signal: None,
                sleep: None,
                persist_blocks: true,
                agent_dir: None,
                rechecked_accounts: None,
            },
            &mut boundary,
        )
        .await
        .expect_err("no usable account");
        let failure = result
            .downcast_ref::<ClassifiedSdkError>()
            .expect("SDK error");
        assert_eq!(failure.to_string(), "authentication_failed: token revoked");
        let pool = failure.pool_state.as_deref().expect("blocked pool");
        assert_eq!(pool.soonest_unblock_at, Some(3000.0));
        assert_eq!(boundary.attempts, ["a"]);
        assert_eq!(boundary.failovers.len(), 1);
        assert!(boundary.failovers[0].next_account.is_none());
    }
}
