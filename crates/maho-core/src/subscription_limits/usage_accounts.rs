//! Port of senpi packages/coding-agent/src/core/subscription-limits/usage-accounts.ts.
//!
//! The subscription accounts a usage poll can speak for, named exactly as turns name them in the store
//! (senpi#3684): a Claude account by its slot name, a ChatGPT account by its pool slot name (`default`
//! for a single login). The token is read only when a poll is actually due, through the credential
//! store, and an expiring Claude token is renewed by the same refresh a turn would run.
//!
//! deviation: maho-ext-anthropic-subscription depends on maho-core, so its account helpers cannot be
//! imported here without a cycle; the few helpers this module needs (slot listing, env slot token,
//! `refresh_slot`, `is_grant_rejected`, the auth-block re-probe interval) are small local ports. The
//! refresher is injected through `AccountSources::refresher`, and the poller's `AbortSignal` (a watch
//! receiver) is bridged to the pi-ai abort signal the refresher takes.
//!
//! claude CLI accounts (senpi#4436, v2026.11.19-5): such an account polls with the access token the CLI keeps in its
//! own store, read as it stands and never refreshed; `read_claude_cli_access_token` ports
//! `claude-cli-account-dir.ts` `readClaudeCliAccessToken` here for the same cycle reason. On macOS the CLI keeps the
//! token in the Keychain, so a macOS account offers none and is not polled.

use std::sync::Arc;

use maho_ai::auth::types::{Credential, CredentialStore, OAuthCredential};
use maho_ai::types::BoxFuture;
use maho_ai::utils::abort::AbortController;
use serde_json::{Value, json};

use super::usage_endpoints::UsageProvider;
use super::usage_poller::AbortSignal;
pub use super::usage_poller::{AccessFuture, AccessResult, PollAccount, SIGN_IN_NEEDED};
use crate::lockfile_policy::CredentialStoreBusyError;

const EXPIRING_WITHIN_MS: f64 = 5.0 * 60_000.0;
/// How long a rejected credential sits out before one request probes it again (senpi#3264).
pub const AUTH_BLOCK_REPROBE_MS: u64 = 15 * 60_000;
const SENTINEL_TOKEN: &str = "claude-sdk-oauth-managed";

/// Renews one Claude slot: `(refresh token, abort signal) -> renewed credential`.
pub type SlotRefresher = Arc<
    dyn Fn(
            String,
            maho_ai::utils::abort::AbortSignal,
        ) -> BoxFuture<'static, anyhow::Result<OAuthCredential>>
        + Send
        + Sync,
>;

pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Clone)]
pub struct AccountSources {
    pub store: Arc<dyn CredentialStore>,
    pub env: EnvLookup,
    pub refresher: SlotRefresher,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// Where the claude CLI account dirs live; without it, claude CLI accounts are not polled.
    pub agent_dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
struct ClaudeSlot {
    name: String,
    is_env: bool,
    /// A claude CLI account's dir: its token is the CLI's own, never refreshed here.
    cli_account_dir: Option<std::path::PathBuf>,
    access: String,
    refresh: String,
    expires: f64,
    block_reason: Option<String>,
}

fn text(slot: &Value, key: &str) -> String {
    slot.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn is_valid_account_dir_id(dir_id: &str) -> bool {
    dir_id.len() == 32
        && dir_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A claude CLI slot's account dir, or `None` when it cannot be polled: no agent dir, a sign-in it must redo, or a
/// dir id that names no directory.
fn cli_account_dir(
    slot: &Value,
    agent_dir: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let dir_id = text(slot, "dirId");
    if slot
        .get("signInRequired")
        .is_some_and(|reason| !reason.is_null())
        || !is_valid_account_dir_id(&dir_id)
    {
        return None;
    }
    Some(crate::config::claude_cli_account_root(agent_dir?).join(dir_id))
}

fn stored_claude_slots(
    credential: &OAuthCredential,
    agent_dir: Option<&std::path::Path>,
) -> Vec<ClaudeSlot> {
    let Some(accounts) = credential.extra.get("accounts").and_then(Value::as_array) else {
        return Vec::new();
    };
    accounts
        .iter()
        .filter(|slot| {
            !(text(slot, "access") == SENTINEL_TOKEN && text(slot, "refresh") == SENTINEL_TOKEN)
        })
        .filter(|slot| !text(slot, "name").trim().is_empty())
        .filter_map(|slot| {
            let is_cli = slot.get("store").and_then(Value::as_str) == Some("claude-cli");
            let cli_account_dir = if is_cli {
                Some(cli_account_dir(slot, agent_dir)?)
            } else {
                None
            };
            Some((slot, cli_account_dir))
        })
        .map(|(slot, cli_account_dir)| ClaudeSlot {
            name: text(slot, "name"),
            is_env: slot.get("source").and_then(Value::as_str) == Some("env"),
            cli_account_dir,
            access: text(slot, "access"),
            refresh: text(slot, "refresh"),
            expires: slot.get("expires").and_then(Value::as_f64).unwrap_or(0.0),
            block_reason: slot
                .get("blockReason")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
        .collect()
}

fn env_claude_slots(env: &(dyn Fn(&str) -> Option<String> + Send + Sync)) -> Vec<ClaudeSlot> {
    (1..=16)
        .filter_map(|index| {
            let key = if index == 1 {
                "CLAUDE_CODE_OAUTH_TOKEN".to_owned()
            } else {
                format!("CLAUDE_CODE_OAUTH_TOKEN_{index}")
            };
            env(&key)
                .filter(|value| !value.is_empty())
                .map(|_| ClaudeSlot {
                    name: if index == 1 {
                        "env".to_owned()
                    } else {
                        format!("env-{index}")
                    },
                    is_env: true,
                    cli_account_dir: None,
                    access: String::new(),
                    refresh: String::new(),
                    expires: 0.0,
                    block_reason: None,
                })
        })
        .collect()
}

fn env_slot_token(
    env: &(dyn Fn(&str) -> Option<String> + Send + Sync),
    name: &str,
) -> Option<String> {
    if name == "env" {
        return env("CLAUDE_CODE_OAUTH_TOKEN");
    }
    let suffix = name.strip_prefix("env-")?;
    if suffix.is_empty() || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    env(&format!("CLAUDE_CODE_OAUTH_TOKEN_{suffix}"))
}

/// Throttling, server errors and timeouts on the token endpoint are not a verdict on the grant
/// (senpi `isGrantRejected` over `refreshFailure`).
pub fn is_grant_rejected(error: &anyhow::Error) -> bool {
    grant_rejected_message(&error.to_string())
}

fn grant_rejected_message(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    let matches = |pattern: &str| regex::Regex::new(pattern).is_ok_and(|re| re.is_match(&lower));
    if matches(r"\bstatus=5\d\d\b") || detail.contains("TimeoutError") {
        return false;
    }
    if matches(
        r"\b(enotfound|eai_again|econnreset|econnrefused|etimedout|enetunreach|ehostunreach|und_err_connect_timeout|und_err_socket)\b|fetch failed|socket hang up|connection reset by peer",
    ) {
        return false;
    }
    for (code, rejected) in [
        ("authentication_failed", true),
        ("oauth_org_not_allowed", true),
        ("billing_error", true),
        ("rate_limit", false),
        ("overloaded", false),
        ("invalid_request", true),
        ("server_error", false),
        ("account_on_hold", true),
        ("model_not_found", true),
        ("max_output_tokens", true),
    ] {
        if matches(&format!(r"\b{code}\b")) {
            return rejected;
        }
    }
    if matches(
        r"\b(?:http\s*)?429\b|too many requests|rate[ _-]?limit|\bblocking_limit\b|\brapid_refill_breaker\b|\b(?:weekly|daily|hourly|usage)\s+limit\b|\b(?:hit|reached|exceeded)\b[^.]*\blimit\b",
    ) {
        return false;
    }
    !matches(r"\b(?:http\s*)?529\b|overloaded")
}

pub async fn list_poll_accounts(sources: &AccountSources) -> Vec<PollAccount> {
    let claude = sources
        .store
        .read("anthropic-subscription", None)
        .await
        .ok()
        .flatten();
    let chatgpt = sources
        .store
        .read("chatgpt-subscription", None)
        .await
        .ok()
        .flatten();
    let mut accounts = claude_accounts(claude, sources);
    accounts.extend(chatgpt_accounts(chatgpt, Arc::clone(&sources.now)));
    accounts
}

fn claude_accounts(stored: Option<Credential>, sources: &AccountSources) -> Vec<PollAccount> {
    let mut slots = match &stored {
        Some(Credential::OAuth(credential)) => {
            stored_claude_slots(credential, sources.agent_dir.as_deref())
        }
        _ => Vec::new(),
    };
    slots.extend(env_claude_slots(&*sources.env));
    slots
        .into_iter()
        .map(|slot| {
            let sources = sources.clone();
            PollAccount {
                provider: UsageProvider::AnthropicSubscription,
                account: slot.name.clone(),
                access: Arc::new(move |signal| {
                    let sources = sources.clone();
                    let slot = slot.clone();
                    Box::pin(async move { claude_access(&sources, &slot, signal).await })
                }),
            }
        })
        .collect()
}

async fn claude_access(
    sources: &AccountSources,
    slot: &ClaudeSlot,
    signal: AbortSignal,
) -> AccessResult {
    if slot.is_env {
        return env_slot_token(&*sources.env, &slot.name).map(Ok);
    }
    if let Some(account_dir) = &slot.cli_account_dir {
        return read_claude_cli_access_token(account_dir, (sources.now)() as f64).map(Ok);
    }
    let expiring = |now: i64, expires: f64| now as f64 >= expires - EXPIRING_WITHIN_MS;
    if !expiring((sources.now)(), slot.expires) {
        return Some(Ok(slot.access.clone()));
    }
    // A login the token endpoint already rejected is not re-redeemed here; a turn owns that recovery.
    if slot.block_reason.as_deref() == Some("auth_error") {
        return Some(Err(SIGN_IN_NEEDED.to_owned()));
    }
    match refresh_slot(sources, &slot.name, &signal).await {
        Ok(Some(Credential::OAuth(refreshed))) => {
            let renewed = stored_claude_slots(&refreshed, None)
                .into_iter()
                .find(|candidate| candidate.name == slot.name);
            match renewed {
                Some(renewed) if !expiring((sources.now)(), renewed.expires) => {
                    Some(Ok(renewed.access))
                }
                _ => Some(Err(SIGN_IN_NEEDED.to_owned())),
            }
        }
        Ok(_) => Some(Err(SIGN_IN_NEEDED.to_owned())),
        Err(error) => {
            // An aborted refresh (shutdown) and a busy store say nothing about the grant: retried next
            // interval, never a lock. A rejected grant is recorded on the slot the way a turn records it,
            // so later polls stop at the auth_error branch above; only /login lifts it.
            if *signal.borrow() || is_busy(&error) || !token_endpoint_rejected(&error) {
                return None;
            }
            // The lock could not be written (a busy store): the next pass asks again and records it then.
            let _ = mark_auth_rejected(sources, &slot.name, &slot.refresh).await;
            Some(Err(SIGN_IN_NEEDED.to_owned()))
        }
    }
}

/// senpi `readClaudeCliAccessToken`: the access token the claude CLI keeps for an account dir, as it stands: read
/// only, never refreshed (senpi#4436). `None` when the credential file is missing, unreadable, malformed or already
/// expired; the CLI renews it on its next turn. It follows no link: a linked account dir or credential file offers
/// none, including one swapped in after the check.
pub fn read_claude_cli_access_token(account_dir: &std::path::Path, now: f64) -> Option<String> {
    let file = account_dir.join(".credentials.json");
    let dir_before = std::fs::symlink_metadata(account_dir).ok()?;
    let file_before = std::fs::symlink_metadata(&file).ok()?;
    if !dir_before.is_dir() || !file_before.is_file() {
        return None;
    }
    let contents = read_unlinked(&file, &file_before)?;
    // The dir is not opened, so a dir swapped for a link during the read shows up as a changed dir afterwards.
    let dir_after = std::fs::symlink_metadata(account_dir).ok()?;
    if !dir_after.is_dir() || !same_file(&dir_before, &dir_after) {
        return None;
    }
    let parsed: Value = serde_json::from_str(&contents).ok()?;
    let oauth = parsed.get("claudeAiOauth")?.as_object()?;
    let access = oauth
        .get("accessToken")?
        .as_str()
        .filter(|token| !token.is_empty())?;
    if oauth
        .get("expiresAt")
        .and_then(Value::as_f64)
        .is_some_and(|expires| expires <= now)
    {
        return None;
    }
    Some(access.to_owned())
}

/// Checked, then read: the open refuses a final link where the platform can, and the opened handle must be the very
/// file that was checked.
fn read_unlinked(file: &std::path::Path, checked: &std::fs::Metadata) -> Option<String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
    }
    let mut handle = options.open(file).ok()?;
    let opened = handle.metadata().ok()?;
    if !opened.is_file() || !same_file(&opened, checked) {
        return None;
    }
    let mut contents = String::new();
    handle.read_to_string(&mut contents).ok()?;
    Some(contents)
}

#[cfg(unix)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.ino() == right.ino() && left.dev() == right.dev()
}
#[cfg(not(unix))]
fn same_file(_left: &std::fs::Metadata, _right: &std::fs::Metadata) -> bool {
    true
}

/// A background poll records the auth_error lock only on an explicit rejection from the token endpoint:
/// a 400/401 answer, or invalid_grant when no status is known (senpi#3695).
pub fn token_endpoint_rejected(error: &anyhow::Error) -> bool {
    if !is_grant_rejected(error) {
        return false;
    }
    static STATUS_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = STATUS_RE.get_or_init(|| regex::Regex::new(r"\bstatus=(\d{3})\b").expect("regex"));

    for cause in error.chain() {
        let msg = cause.to_string();
        if let Some(caps) = re.captures(&msg)
            && let Some(status) = caps.get(1)
        {
            let s = status.as_str();
            return s == "400" || s == "401";
        }
        if msg.contains("invalid_grant") {
            return true;
        }
    }
    let full = error.to_string();
    if let Some(caps) = re.captures(&full)
        && let Some(status) = caps.get(1)
    {
        let s = status.as_str();
        return s == "400" || s == "401";
    }
    full.contains("invalid_grant")
}

fn is_busy(error: &anyhow::Error) -> bool {
    error.downcast_ref::<CredentialStoreBusyError>().is_some()
        || error.to_string().starts_with("Credential store is busy")
}

/// Renews one stored Claude slot under the store's serialized write (senpi `refreshSlot`).
async fn refresh_slot(
    sources: &AccountSources,
    name: &str,
    signal: &AbortSignal,
) -> anyhow::Result<Option<Credential>> {
    let controller = AbortController::new();
    let bridged = controller.signal();
    let mut watch = signal.clone();
    let bridge = tokio::spawn(async move {
        while !*watch.borrow() {
            if watch.changed().await.is_err() {
                return;
            }
        }
        controller.abort(None);
    });
    let name = name.to_owned();
    let now = (sources.now)() as f64;
    let refresher = Arc::clone(&sources.refresher);
    let result = sources
        .store
        .modify(
            "anthropic-subscription",
            Box::new(move |current| {
                Box::pin(async move {
                    let Some(Credential::OAuth(mut credential)) = current else {
                        return Ok(None);
                    };
                    let mut accounts = credential
                        .extra
                        .get("accounts")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    let Some(slot) = accounts
                        .iter()
                        .find(|slot| text(slot, "name") == name)
                        .cloned()
                    else {
                        return Ok(Some(Credential::OAuth(credential)));
                    };
                    let expires = slot.get("expires").and_then(Value::as_f64).unwrap_or(0.0);
                    if now < expires - EXPIRING_WITHIN_MS {
                        return Ok(Some(Credential::OAuth(credential)));
                    }
                    let refreshed = refresher(text(&slot, "refresh"), bridged).await?;
                    for slot in accounts
                        .iter_mut()
                        .filter(|slot| text(slot, "name") == name)
                    {
                        if let Some(object) = slot.as_object_mut() {
                            object.insert("access".to_owned(), json!(refreshed.access));
                            object.insert("refresh".to_owned(), json!(refreshed.refresh));
                            object.insert("expires".to_owned(), json!(refreshed.expires));
                        }
                    }
                    credential
                        .extra
                        .insert("accounts".to_owned(), Value::Array(accounts));
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await;
    bridge.abort();
    result
}

/// Locks the slot only while it still holds the grant that was rejected: a /login, a sibling's refresh
/// or a turn's recovery may have written new material since, and that token must not be locked.
async fn mark_auth_rejected(
    sources: &AccountSources,
    name: &str,
    rejected_refresh: &str,
) -> anyhow::Result<()> {
    let now = (sources.now)().max(0) as u64;
    let name = name.to_owned();
    let rejected_refresh = rejected_refresh.to_owned();
    sources
        .store
        .modify(
            "anthropic-subscription",
            Box::new(move |current| {
                Box::pin(async move {
                    let mut credential = match current {
                        Some(Credential::OAuth(credential)) => credential,
                        other => return Ok(other),
                    };
                    let Some(mut accounts) = credential
                        .extra
                        .get("accounts")
                        .and_then(Value::as_array)
                        .cloned()
                    else {
                        return Ok(Some(Credential::OAuth(credential)));
                    };
                    if !accounts.iter().any(|slot| {
                        text(slot, "name") == name && text(slot, "refresh") == rejected_refresh
                    }) {
                        return Ok(Some(Credential::OAuth(credential)));
                    }
                    for slot in accounts
                        .iter_mut()
                        .filter(|slot| text(slot, "name") == name)
                    {
                        if let Some(object) = slot.as_object_mut() {
                            object.insert(
                                "blockedUntil".to_owned(),
                                json!(now + AUTH_BLOCK_REPROBE_MS),
                            );
                            object.insert("blockReason".to_owned(), json!("auth_error"));
                            object.insert("blockedAt".to_owned(), json!(now));
                        }
                    }
                    credential
                        .extra
                        .insert("accounts".to_owned(), Value::Array(accounts));
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await
        .map(|_| ())
}

fn chatgpt_accounts(
    stored: Option<Credential>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
) -> Vec<PollAccount> {
    let Some(Credential::OAuth(credential)) = stored else {
        return Vec::new();
    };
    let pooled: Vec<(String, String, Option<f64>)> =
        match credential.extra.get("accounts").and_then(Value::as_array) {
            Some(accounts) if !accounts.is_empty() => accounts
                .iter()
                .filter(|slot| !text(slot, "name").is_empty())
                .map(|slot| {
                    (
                        text(slot, "name"),
                        text(slot, "access"),
                        slot.get("expires").and_then(Value::as_f64),
                    )
                })
                .collect(),
            _ => vec![(
                "default".to_owned(),
                credential.access.clone(),
                Some(credential.expires),
            )],
        };
    pooled
        .into_iter()
        .filter(|(_, access, _)| !access.is_empty())
        .map(|(name, access, expires)| {
            let now = Arc::clone(&now);
            PollAccount {
                provider: UsageProvider::ChatgptSubscription,
                account: name,
                access: Arc::new(move |_signal| {
                    // An expired token is not renewed here; a turn renews it.
                    let usable = expires.is_none_or(|expires| ((now)() as f64) < expires);
                    let result: AccessResult = usable.then(|| Ok(access.clone()));
                    Box::pin(async move { result })
                }),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttling_and_server_failures_are_not_a_rejected_grant() {
        assert!(!grant_rejected_message("rate_limit: slow down"));
        assert!(!grant_rejected_message("token endpoint status=503"));
        assert!(!grant_rejected_message("fetch failed"));
        assert!(!grant_rejected_message("TimeoutError: request timed out"));
    }

    #[test]
    fn a_rejected_grant_is_recognized() {
        assert!(grant_rejected_message(
            "invalid_grant: refresh token revoked"
        ));
        assert!(grant_rejected_message("authentication_failed: 401"));
    }

    #[test]
    fn chatgpt_accounts_skip_empty_access_and_expired_tokens_yield_none() {
        let credential = Credential::OAuth(OAuthCredential::new("tok", "ref", 1_000.0));
        let accounts = chatgpt_accounts(Some(credential), Arc::new(|| 5_000));
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].account, "default");
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let result = futures::executor::block_on((accounts[0].access)(rx));
        assert_eq!(result, None);
    }

    #[test]
    fn env_slots_name_like_turns_do() {
        let env: EnvLookup = Arc::new(|key| {
            matches!(key, "CLAUDE_CODE_OAUTH_TOKEN" | "CLAUDE_CODE_OAUTH_TOKEN_2")
                .then(|| "secret".to_owned())
        });
        let names: Vec<String> = env_claude_slots(&*env)
            .into_iter()
            .map(|slot| slot.name)
            .collect();
        assert_eq!(names, ["env", "env-2"]);
        assert_eq!(env_slot_token(&*env, "env-2").as_deref(), Some("secret"));
    }

    #[cfg(unix)]
    #[test]
    fn claude_cli_accounts_poll_with_the_cli_token_as_it_stands_and_never_through_a_link() {
        let temp = tempfile::tempdir().expect("temp");
        let dir_id = "0123456789abcdef0123456789abcdef";
        let account_dir = temp
            .path()
            .join("anthropic-subscription-claude")
            .join(dir_id);
        std::fs::create_dir_all(&account_dir).expect("account dir");
        let credential = OAuthCredential::new(SENTINEL_TOKEN, SENTINEL_TOKEN, 1.0).with_extra(
            "accounts",
            json!([
                {"name":"cli","source":"login","store":"claude-cli","dirId":dir_id},
                {"name":"expired","source":"login","store":"claude-cli","dirId":dir_id,"signInRequired":"login_expired"},
                {"name":"broken","source":"login","store":"claude-cli","dirId":"nope"}
            ]),
        );
        assert!(stored_claude_slots(&credential, None).is_empty());
        let slots = stored_claude_slots(&credential, Some(temp.path()));
        assert_eq!(slots.len(), 1);
        assert_eq!(
            slots[0].cli_account_dir.as_deref(),
            Some(account_dir.as_path())
        );
        assert_eq!(read_claude_cli_access_token(&account_dir, 10.0), None);
        let file = account_dir.join(".credentials.json");
        std::fs::write(
            &file,
            r#"{"claudeAiOauth":{"accessToken":"synthetic","expiresAt":100}}"#,
        )
        .expect("credentials");
        assert_eq!(
            read_claude_cli_access_token(&account_dir, 10.0).as_deref(),
            Some("synthetic")
        );
        assert_eq!(read_claude_cli_access_token(&account_dir, 100.0), None);
        let elsewhere = temp.path().join("elsewhere.json");
        std::fs::rename(&file, &elsewhere).expect("move");
        std::os::unix::fs::symlink(&elsewhere, &file).expect("link");
        assert_eq!(read_claude_cli_access_token(&account_dir, 10.0), None);
    }

    #[test]
    fn token_endpoint_rejected_classification() {
        let err_400 = anyhow::anyhow!("token refresh failed status=400 bad request");
        assert!(token_endpoint_rejected(&err_400));

        let err_401 = anyhow::anyhow!("token refresh failed status=401 unauthorized");
        assert!(token_endpoint_rejected(&err_401));

        let err_invalid_grant = anyhow::anyhow!("error: invalid_grant description=token revoked");
        assert!(token_endpoint_rejected(&err_invalid_grant));

        let err_500 = anyhow::anyhow!("token refresh failed status=500 server error");
        assert!(!token_endpoint_rejected(&err_500));

        let err_network = anyhow::anyhow!("connection reset by peer");
        assert!(!token_endpoint_rejected(&err_network));
    }
}
