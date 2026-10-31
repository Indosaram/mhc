//! senpi `account-context.ts`: which account an SDK attempt ran on, for billing guidance and the session log.
//!
//! Upstream tags each SDK event and error object through a `WeakMap`; this port carries the context of the attempt
//! that produced the error alongside it in the stream loop (`stream.rs`), which is the same attribution: overlapping
//! turns and failover never borrow another attempt's account.
use crate::{accounts::AccountSlot, affinity::PinSource, auth_lane::TokenInjection};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountContextSource {
    Login,
    Import,
    Env,
    Ambient,
    Unknown,
}

impl AccountContextSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Import => "import",
            Self::Env => "env",
            Self::Ambient => "ambient",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountContext {
    pub account_label: String,
    pub account_source: AccountContextSource,
    pub pin_source: Option<PinSource>,
}

/// The fields of upstream `AuthenticatedAttemptInput` the context reads.
pub struct AttemptAccount<'a> {
    pub account_name: &'a str,
    pub accounts: &'a [AccountSlot],
    pub auth_lane: TokenInjection,
    pub pin_source: Option<PinSource>,
}

fn plain_account_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && name.len() <= 64
        && characters.all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        })
}

pub fn attempt_account_context(auth: &AttemptAccount<'_>) -> AccountContext {
    AccountContext {
        // Operational account names, unlike displayName, cannot contain an email or org identifier.
        account_label: if plain_account_name(auth.account_name) {
            auth.account_name.to_owned()
        } else {
            "configured account".to_owned()
        },
        account_source: if auth.auth_lane == TokenInjection::Ambient {
            AccountContextSource::Ambient
        } else {
            auth.accounts
                .iter()
                .find(|account| account.name == auth.account_name)
                .map_or(AccountContextSource::Unknown, |account| {
                    match account.source {
                        crate::accounts::AccountSource::Login => AccountContextSource::Login,
                        crate::accounts::AccountSource::Import => AccountContextSource::Import,
                        crate::accounts::AccountSource::Env => AccountContextSource::Env,
                    }
                })
        },
        pin_source: auth.pin_source,
    }
}

/// `logAttemptAccount`'s payload: which agent dir (digested) and seat the attempt authenticates from.
pub fn attempt_account_log_data(
    auth: &AttemptAccount<'_>,
    session_id: Option<&str>,
    agent_dir: &str,
    environment: &std::collections::HashMap<String, String>,
) -> Map<String, Value> {
    let profile = maho_core::brand::brand_profile();
    // This is the config resolver's exact prefix order, including explicitly empty selectors.
    let selector = maho_core::brand::brand_env_names("CODING_AGENT_DIR", Some(&profile))
        .into_iter()
        .find(|name| environment.contains_key(name));
    let selected = selector
        .as_ref()
        .is_some_and(|name| environment.get(name).is_some_and(|value| !value.is_empty()));
    let home = maho_core::config::home_dir();
    let config_dir = maho_core::config::config_dir_name();
    let default_dir = if maho_core::config::config_flat_layout() {
        std::path::Path::new(&home).join(&config_dir)
    } else {
        std::path::Path::new(&home).join(&config_dir).join("agent")
    };
    let is_default = std::path::Path::new(agent_dir) == default_dir;
    let directory = std::path::Path::new(agent_dir);
    let mut data = Map::new();
    if let Some(session_id) = session_id {
        data.insert("sessionId".into(), json!(session_id));
    }
    data.insert(
        "agentDir".into(),
        json!(format!(
            "dir:{}",
            crate::seat_digest::seat_digest(&format!("agent-dir:{agent_dir}"), Some(directory))
        )),
    );
    data.insert(
        "agentDirSource".into(),
        json!(match (&selector, selected, is_default) {
            (Some(name), true, _) => name.as_str(),
            (_, _, true) => "default",
            _ => "project",
        }),
    );
    data.insert(
        "agentDirRule".into(),
        json!(if selected {
            "environment"
        } else if is_default {
            "home-default"
        } else {
            "project-local"
        }),
    );
    data.insert(
        "seat".into(),
        json!(crate::seat_digest::seat_digest(
            auth.account_name,
            Some(directory)
        )),
    );
    data.insert(
        "accountSource".into(),
        json!(attempt_account_context(auth).account_source.as_str()),
    );
    data
}

/// `logAttemptAccount`: the `claude_sdk_oauth_session_auth` continuity event, written to the session log of the
/// extension's agent dir (upstream reads the process `getAgentDir()`; the native extension carries its own).
pub fn log_attempt_account(
    auth: &AttemptAccount<'_>,
    session_id: Option<&str>,
    agent_dir: &std::path::Path,
) {
    let agent_dir = agent_dir.to_string_lossy();
    let environment: std::collections::HashMap<String, String> = std::env::vars().collect();
    let data = attempt_account_log_data(auth, session_id, &agent_dir, &environment);
    maho_core::session_log::SessionLogger::create(Some(&agent_dir), Default::default())
        .info("claude_sdk_oauth_session_auth", Some(&data));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountSource;

    fn slot(name: &str, source: AccountSource) -> AccountSlot {
        AccountSlot {
            name: name.into(),
            display_name: Some("someone@example.com".into()),
            refresh: String::new(),
            access: String::new(),
            expires: 0.0,
            source,
            blocked_until: None,
            block_reason: None,
            ..Default::default()
        }
    }

    #[test]
    fn labels_only_plain_names_and_reads_the_slot_source() {
        let accounts = [
            slot("work", AccountSource::Import),
            slot("me@example.com", AccountSource::Login),
        ];
        let context = attempt_account_context(&AttemptAccount {
            account_name: "work",
            accounts: &accounts,
            auth_lane: TokenInjection::OAuthSlots,
            pin_source: Some(PinSource::Cli),
        });
        assert_eq!(context.account_label, "work");
        assert_eq!(context.account_source, AccountContextSource::Import);
        assert_eq!(context.pin_source, Some(PinSource::Cli));
        let private = attempt_account_context(&AttemptAccount {
            account_name: "me@example.com",
            accounts: &accounts,
            auth_lane: TokenInjection::OAuthSlots,
            pin_source: None,
        });
        assert_eq!(private.account_label, "configured account");
        assert_eq!(private.account_source, AccountContextSource::Login);
        let missing = attempt_account_context(&AttemptAccount {
            account_name: "gone",
            accounts: &accounts,
            auth_lane: TokenInjection::OAuthSlots,
            pin_source: None,
        });
        assert_eq!(missing.account_source, AccountContextSource::Unknown);
        let ambient = attempt_account_context(&AttemptAccount {
            account_name: "work",
            accounts: &accounts,
            auth_lane: TokenInjection::Ambient,
            pin_source: None,
        });
        assert_eq!(ambient.account_source, AccountContextSource::Ambient);
        assert_eq!(
            attempt_account_context(&AttemptAccount {
                account_name: &"x".repeat(65),
                accounts: &[],
                auth_lane: TokenInjection::Ambient,
                pin_source: None
            })
            .account_label,
            "configured account"
        );
    }

    #[test]
    fn session_auth_event_digests_the_agent_dir_and_names_the_rule() {
        let directory = tempfile::tempdir().expect("directory");
        let agent_dir = directory.path().to_string_lossy().into_owned();
        let auth = AttemptAccount {
            account_name: "work",
            accounts: &[],
            auth_lane: TokenInjection::OAuthSlots,
            pin_source: None,
        };
        let data = attempt_account_log_data(&auth, Some("s"), &agent_dir, &Default::default());
        assert_eq!(data["sessionId"], "s");
        assert!(data["agentDir"].as_str().unwrap().starts_with("dir:"));
        assert!(!data["agentDir"].as_str().unwrap().contains(&agent_dir));
        assert_eq!(data["agentDirSource"], "project");
        assert_eq!(data["agentDirRule"], "project-local");
        assert_eq!(data["seat"].as_str().unwrap().len(), 8);
        assert_eq!(data["accountSource"], "unknown");
        let name = maho_core::brand::brand_env_names(
            "CODING_AGENT_DIR",
            Some(&maho_core::brand::brand_profile()),
        )
        .remove(0);
        let selected = attempt_account_log_data(
            &auth,
            None,
            &agent_dir,
            &[(name.clone(), agent_dir.clone())].into(),
        );
        assert!(selected.get("sessionId").is_none());
        assert_eq!(selected["agentDirSource"], name.as_str());
        assert_eq!(selected["agentDirRule"], "environment");
        let empty =
            attempt_account_log_data(&auth, None, &agent_dir, &[(name, String::new())].into());
        assert_eq!(empty["agentDirRule"], "project-local");
    }
}
