//! Port of senpi packages/coding-agent/src/core/subscription-limits/usage-accounts.ts.
//!
//! The subscription accounts a usage poll can speak for, named exactly as turns name them in the store
//! (senpi#3684): a Claude account by its slot name, a ChatGPT account by its pool slot name (`default`
//! for a single login). The token is read only when a poll is actually due. A Claude account polls with its
//! `CLAUDE_CODE_OAUTH_TOKEN` env token, or, for a claude CLI account, with the access token the CLI keeps in its own
//! store, read as it stands: senpi never refreshes a Claude token (senpi#4436, v2026.11.19-5), so an expired one
//! waits for the CLI's next turn, and a slot of the retired token login is not polled.
//!
//! deviation: maho-ext-anthropic-subscription depends on maho-core, so its account helpers cannot be
//! imported here without a cycle; the few helpers this module needs (slot listing, env slot token, the auth-block
//! re-probe interval) are small local ports. `read_claude_cli_access_token` ports
//! `claude-cli-account-dir.ts` `readClaudeCliAccessToken` here for the same cycle reason. On macOS the CLI keeps the
//! token in the Keychain, so a macOS account offers none and is not polled.

use std::sync::Arc;

use maho_ai::auth::types::{Credential, CredentialStore, OAuthCredential};
use serde_json::Value;

use super::usage_endpoints::UsageProvider;
pub use super::usage_poller::{AccessFuture, AccessResult, PollAccount, SIGN_IN_NEEDED};

/// How long a rejected credential sits out before one request probes it again (senpi#3264).
pub const AUTH_BLOCK_REPROBE_MS: u64 = 15 * 60_000;

pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Clone)]
pub struct AccountSources {
    pub store: Arc<dyn CredentialStore>,
    pub env: EnvLookup,
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
        .filter(|slot| !text(slot, "name").trim().is_empty())
        .filter(|slot| slot.get("store").and_then(Value::as_str) == Some("claude-cli"))
        .filter_map(|slot| {
            Some(ClaudeSlot {
                name: text(slot, "name"),
                is_env: false,
                cli_account_dir: Some(cli_account_dir(slot, agent_dir)?),
            })
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
                access: Arc::new(move |_signal| {
                    let result = claude_access(&sources, &slot);
                    Box::pin(async move { result })
                }),
            }
        })
        .collect()
}

fn claude_access(sources: &AccountSources, slot: &ClaudeSlot) -> AccessResult {
    if slot.is_env {
        return env_slot_token(&*sources.env, &slot.name).map(Ok);
    }
    let account_dir = slot.cli_account_dir.as_ref()?;
    read_claude_cli_access_token(account_dir, (sources.now)() as f64).map(Ok)
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
    use serde_json::json;

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
        let credential = OAuthCredential::new("claude-sdk-oauth-managed", "claude-sdk-oauth-managed", 1.0).with_extra(
            "accounts",
            json!([
                {"name":"cli","source":"login","store":"claude-cli","dirId":dir_id},
                {"name":"expired","source":"login","store":"claude-cli","dirId":dir_id,"signInRequired":"login_expired"},
                {"name":"broken","source":"login","store":"claude-cli","dirId":"nope"},
                {"name":"token","source":"login","access":"a","refresh":"r","expires":1}
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
}
