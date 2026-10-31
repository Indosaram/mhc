use crate::{
    accounts::{BlockReason, CursorCliAccountSlot, list_accounts},
    affinity::{CursorAffinityOptions, select_account},
    errors::{
        CursorCliErrorClassification, CursorCliErrorInput, CursorCliErrorKind,
        DEFAULT_RATE_LIMIT_BLOCK_MS, MAX_RATE_LIMIT_BLOCK_MS, classify_cursor_cli_error,
    },
    session_router::AttemptReceiver,
};
use maho_ai::auth::types::{Credential, CredentialStore};
use serde_json::{Value, json};
pub struct FailoverError {
    pub classification: CursorCliErrorClassification,
    pub original: Value,
    pub visible_assistant_delta_emitted: bool,
}
pub enum RunError {
    Attempt(FailoverError),
    Store(anyhow::Error),
    AllBlocked(crate::affinity::AllCursorAccountsBlockedError),
}
async fn select_stored(
    store: &dyn CredentialStore,
    provider: &str,
    affinity: &CursorAffinityOptions<'_>,
    now: f64,
) -> Result<CursorCliAccountSlot, RunError> {
    let current = store.read(provider, None).await.map_err(RunError::Store)?;
    let credential = current.as_ref().and_then(Credential::as_oauth);
    let accounts = credential
        .map(list_accounts)
        .transpose()
        .map_err(RunError::Store)?
        .unwrap_or_default();
    let order = credential.and_then(crate::accounts::account_order);
    let options = CursorAffinityOptions {
        pinned_account: affinity
            .pinned_account
            .or_else(|| credential.and_then(|c| c.get_extra_str("pinned"))),
        affinity_key: affinity.affinity_key,
        session_id: affinity.session_id,
        account_order: order.as_deref(),
    };
    select_account(&accounts, &options, now).map_err(RunError::AllBlocked)
}
async fn persist_block(
    store: &dyn CredentialStore,
    provider: &str,
    account: &CursorCliAccountSlot,
    classification: CursorCliErrorClassification,
    now: f64,
) -> anyhow::Result<()> {
    let name = account.name.clone();
    store
        .modify(
            provider,
            Box::new(move |current| {
                Box::pin(async move {
                    let Some(Credential::OAuth(mut credential)) = current else {
                        return Ok(current);
                    };
                    let mut accounts = list_accounts(&credential)?;
                    for slot in &mut accounts {
                        if slot.name != name {
                            continue;
                        }
                        if classification.kind == CursorCliErrorKind::AuthError {
                            slot.block_reason = Some(BlockReason::AuthError);
                            slot.blocked_until = None;
                        } else {
                            slot.block_reason = Some(BlockReason::RateLimit);
                            slot.blocked_until = Some(
                                now + classification
                                    .block_ms
                                    .unwrap_or(DEFAULT_RATE_LIMIT_BLOCK_MS)
                                    .min(MAX_RATE_LIMIT_BLOCK_MS),
                            );
                        }
                    }
                    credential
                        .extra
                        .insert("accounts".into(), serde_json::to_value(accounts)?);
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await?;
    Ok(())
}
pub struct FailoverOptions<'a> {
    pub store: &'a dyn CredentialStore,
    pub provider_id: &'a str,
    pub affinity: CursorAffinityOptions<'a>,
}
pub async fn run_failover<F, Fut, N, O>(
    options: FailoverOptions<'_>,
    mut run: F,
    now: N,
    mut emit: O,
) -> Result<(), RunError>
where
    F: FnMut(CursorCliAccountSlot, bool) -> Fut,
    Fut: std::future::Future<Output = Result<AttemptReceiver, Value>>,
    N: Fn() -> f64,
    O: FnMut(Value),
{
    let mut account =
        select_stored(options.store, options.provider_id, &options.affinity, now()).await?;
    let mut fresh = false;
    loop {
        let mut visible = false;
        let failure = match run(account.clone(), fresh).await {
            Err(error) => error,
            Ok(mut events) => {
                let mut failure = None;
                while let Some(event) = events.recv().await {
                    let event = match event {
                        Ok(e) => e,
                        Err(e) => {
                            failure = Some(e);
                            break;
                        }
                    };
                    if event["type"] == "malformed_stream" {
                        failure = Some(json!({"thrown":event}));
                        break;
                    }
                    if event["type"] == "result"
                        && (event["is_error"] == true || event["subtype"] == "error")
                    {
                        failure = Some(json!({"resultEvent":event}));
                        break;
                    }
                    visible |= (event["type"] == "assistant"
                        && event["message"]["content"]
                            .as_array()
                            .is_some_and(|blocks| {
                                blocks.iter().any(|b| {
                                    b["type"] == "text"
                                        && b["text"].as_str().is_some_and(|s| !s.is_empty())
                                })
                            }))
                        || ((event["type"] == "assistant_delta" || event["type"] == "text_delta")
                            && event["delta"].as_str().is_some_and(|s| !s.is_empty()));
                    emit(event);
                }
                match failure {
                    Some(error) => error,
                    None if visible => return Ok(()),
                    None => {
                        json!({"message":"Cursor CLI attempt completed without visible assistant text"})
                    }
                }
            }
        };
        let input = if ["exitCode", "stderr", "resultEvent", "thrown"]
            .iter()
            .any(|k| failure.get(*k).is_some())
        {
            CursorCliErrorInput {
                exit_code: failure["exitCode"].clone(),
                stderr: failure["stderr"].clone(),
                result_event: failure["resultEvent"].clone(),
                thrown: failure["thrown"].clone(),
            }
        } else {
            CursorCliErrorInput {
                thrown: failure.clone(),
                ..Default::default()
            }
        };
        let classification = classify_cursor_cli_error(Some(&input));
        let surfaced = RunError::Attempt(FailoverError {
            classification,
            original: failure,
            visible_assistant_delta_emitted: visible,
        });
        if !matches!(
            classification.kind,
            CursorCliErrorKind::RateLimit | CursorCliErrorKind::AuthError
        ) {
            return Err(surfaced);
        }
        persist_block(
            options.store,
            options.provider_id,
            &account,
            classification,
            now(),
        )
        .await
        .map_err(RunError::Store)?;
        if visible {
            return Err(surfaced);
        }
        let next =
            select_stored(options.store, options.provider_id, &options.affinity, now()).await?;
        emit(
            json!({"type":"cursor_account_changed","message":format!("Cursor account changed from '{}' to '{}'; a fresh chat was started and prior context was not carried over.",account.name,next.name),"fromAccount":account.name,"toAccount":next.name,"freshChat":true,"priorContextCarriedOver":false}),
        );
        account = next;
        fresh = true;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::{AccountSource, add_account, empty_credential};
    use maho_ai::auth::credential_store::InMemoryCredentialStore;
    async fn store() -> InMemoryCredentialStore {
        let store = InMemoryCredentialStore::new();
        let mut credential = empty_credential();
        for name in ["alpha", "bravo"] {
            credential = add_account(
                &credential,
                CursorCliAccountSlot {
                    name: name.into(),
                    display_name: None,
                    access: "access".into(),
                    refresh: "refresh".into(),
                    expires: 50000.0,
                    source: AccountSource::Login,
                    blocked_until: None,
                    block_reason: None,
                },
            )
            .expect("account");
        }
        store
            .modify(
                "cursor-cli-oauth",
                Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .expect("store");
        store
    }
    #[tokio::test]
    async fn rate_limit_rotates_before_text_and_persists_hint() {
        let store = store().await;
        let mut attempts = Vec::new();
        let mut output = Vec::new();
        let result = run_failover(
            FailoverOptions {
                store: &store,
                provider_id: "cursor-cli-oauth",
                affinity: CursorAffinityOptions {
                    session_id: Some("s"),
                    ..Default::default()
                },
            },
            |account, fresh| {
                attempts.push((account.name, fresh));
                let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                if attempts.len() == 1 {
                    sender
                        .send(Err(
                            json!({"stderr":"HTTP 429 rate limit retry-after-ms: 2500"}),
                        ))
                        .expect("failure");
                } else {
                    sender
                        .send(Ok(json!({"type":"assistant_delta","delta":"answer"})))
                        .expect("answer");
                }
                async move { Ok(receiver) }
            },
            || 10000.0,
            |event| output.push(event),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(attempts.len(), 2);
        assert!(!attempts[0].1);
        assert!(attempts[1].1);
        assert_ne!(attempts[0].0, attempts[1].0);
        assert_eq!(output[0]["priorContextCarriedOver"], false);
        let credential = store
            .read("cursor-cli-oauth", None)
            .await
            .expect("read")
            .expect("credential")
            .into_oauth()
            .expect("oauth");
        let accounts = list_accounts(&credential).expect("accounts");
        let blocked = accounts
            .iter()
            .find(|a| a.name == attempts[0].0)
            .expect("blocked");
        assert_eq!(blocked.blocked_until, Some(12500.0));
    }
    #[tokio::test]
    async fn post_text_limit_persists_block_but_does_not_rotate() {
        let store = store().await;
        let mut attempts = 0;
        let result = run_failover(
            FailoverOptions {
                store: &store,
                provider_id: "cursor-cli-oauth",
                affinity: Default::default(),
            },
            |_, _| {
                attempts += 1;
                let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                sender
                    .send(Ok(json!({"type":"assistant_delta","delta":"partial"})))
                    .expect("partial");
                sender
                    .send(Err(json!({"stderr":"HTTP 429 rate limit"})))
                    .expect("limit");
                async move { Ok(receiver) }
            },
            || 10000.0,
            |_| {},
        )
        .await;
        assert_eq!(attempts, 1);
        assert!(matches!(
            result,
            Err(RunError::Attempt(FailoverError {
                visible_assistant_delta_emitted: true,
                ..
            }))
        ));
    }
    #[tokio::test]
    async fn tries_the_stored_account_order_first_and_fails_over_down_it() {
        let store = store().await;
        let pool = list_accounts(
            store
                .read("cursor-cli-oauth", None)
                .await
                .expect("read")
                .expect("credential")
                .as_oauth()
                .expect("oauth"),
        )
        .expect("accounts");
        let order: Vec<String> = crate::affinity::rendezvous_order("ordered", &pool)
            .into_iter()
            .map(|a| a.name)
            .rev()
            .collect();
        let stored_order = order.clone();
        store
            .modify(
                "cursor-cli-oauth",
                Box::new(move |current| {
                    Box::pin(async move {
                        let Some(Credential::OAuth(credential)) = current else {
                            return Ok(current);
                        };
                        Ok(Some(Credential::OAuth(
                            credential.with_extra("order", json!(stored_order)),
                        )))
                    })
                }),
                None,
            )
            .await
            .expect("order");
        let mut attempts = Vec::new();
        run_failover(
            FailoverOptions {
                store: &store,
                provider_id: "cursor-cli-oauth",
                affinity: CursorAffinityOptions {
                    session_id: Some("ordered"),
                    ..Default::default()
                },
            },
            |account, _| {
                attempts.push(account.name);
                let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                if attempts.len() == 1 {
                    sender
                        .send(Err(
                            json!({"stderr":"HTTP 429 rate limit retry-after-ms: 2500"}),
                        ))
                        .expect("failure");
                } else {
                    sender
                        .send(Ok(json!({"type":"assistant_delta","delta":"answer"})))
                        .expect("answer");
                }
                async move { Ok(receiver) }
            },
            || 10000.0,
            |_| {},
        )
        .await
        .unwrap_or_else(|_| panic!("ordered failover"));
        assert_eq!(attempts, order);
    }

    async fn store_with(slots: Vec<CursorCliAccountSlot>) -> InMemoryCredentialStore {
        let store = InMemoryCredentialStore::new();
        let mut credential = empty_credential();
        for slot in slots {
            credential = add_account(&credential, slot).expect("account");
        }
        store
            .modify(
                "cursor-cli-oauth",
                Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .expect("store");
        store
    }
    fn pool_slot(name: &str) -> CursorCliAccountSlot {
        CursorCliAccountSlot {
            name: name.into(),
            display_name: None,
            access: format!("a-{name}"),
            refresh: format!("r-{name}"),
            expires: 50000.0,
            source: AccountSource::Login,
            blocked_until: None,
            block_reason: None,
        }
    }
    fn one_failure_then(
        attempts: &mut Vec<String>,
        account: CursorCliAccountSlot,
        failure: Value,
    ) -> impl std::future::Future<Output = Result<AttemptReceiver, Value>> + use<> {
        attempts.push(account.name);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        if attempts.len() == 1 {
            sender.send(Err(failure)).expect("failure");
        } else {
            sender
                .send(Ok(json!({"type":"assistant_delta","delta":"recovered"})))
                .expect("answer");
        }
        async move { Ok(receiver) }
    }
    #[tokio::test]
    async fn blocks_auth_errors_until_re_login_and_tries_the_next_account() {
        let store = store_with(["alpha", "bravo", "charlie"].map(pool_slot).into()).await;
        let mut attempts = Vec::new();
        let result = run_failover(
            FailoverOptions {
                store: &store,
                provider_id: "cursor-cli-oauth",
                affinity: CursorAffinityOptions {
                    session_id: Some("auth-error"),
                    ..Default::default()
                },
            },
            |account, _| {
                one_failure_then(
                    &mut attempts,
                    account,
                    json!({"resultEvent":{"result":"HTTP 401 Unauthorized"}}),
                )
            },
            || 10000.0,
            |_| {},
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(attempts.len(), 2);
        let credential = store
            .read("cursor-cli-oauth", None)
            .await
            .expect("read")
            .expect("credential")
            .into_oauth()
            .expect("oauth");
        let blocked = list_accounts(&credential)
            .expect("accounts")
            .into_iter()
            .find(|a| a.name == attempts[0])
            .expect("blocked");
        assert_eq!(blocked.block_reason, Some(BlockReason::AuthError));
        assert_eq!(blocked.blocked_until, None);
    }
    #[tokio::test]
    async fn raises_all_blocked_when_no_account_is_available() {
        let mut slots: Vec<CursorCliAccountSlot> =
            ["alpha", "bravo", "charlie"].map(pool_slot).into();
        slots[0].block_reason = Some(BlockReason::AuthError);
        for (index, slot) in slots.iter_mut().enumerate().skip(1) {
            slot.block_reason = Some(BlockReason::RateLimit);
            slot.blocked_until = Some(10000.0 + index as f64 * 1000.0);
        }
        let store = store_with(slots).await;
        let mut attempts = 0;
        let result = run_failover(
            FailoverOptions {
                store: &store,
                provider_id: "cursor-cli-oauth",
                affinity: CursorAffinityOptions {
                    session_id: Some("all-blocked"),
                    ..Default::default()
                },
            },
            |_, _| {
                attempts += 1;
                let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
                async move { Ok(receiver) }
            },
            || 10000.0,
            |_| {},
        )
        .await;
        assert!(
            matches!(result, Err(RunError::AllBlocked(error)) if error.soonest_unblock_at == Some(11000.0))
        );
        assert_eq!(attempts, 0);
    }
    #[tokio::test]
    async fn never_rotates_non_retryable_failures() {
        for (kind, failure) in [
            (
                CursorCliErrorKind::InvalidModel,
                json!({"resultEvent":{"result":"Invalid model value: bogus"}}),
            ),
            (
                CursorCliErrorKind::BinaryMissing,
                json!({"thrown":{"kind":"binary_missing","message":"missing"}}),
            ),
            (
                CursorCliErrorKind::MalformedStream,
                json!({"thrown":{"kind":"malformed_stream","message":"bad json"}}),
            ),
            (
                CursorCliErrorKind::Other,
                json!({"message":"unknown failure"}),
            ),
        ] {
            let store = store_with(["alpha", "bravo", "charlie"].map(pool_slot).into()).await;
            let mut attempts = Vec::new();
            let result = run_failover(
                FailoverOptions {
                    store: &store,
                    provider_id: "cursor-cli-oauth",
                    affinity: CursorAffinityOptions {
                        session_id: Some("non-retryable"),
                        ..Default::default()
                    },
                },
                |account, _| one_failure_then(&mut attempts, account, failure.clone()),
                || 10000.0,
                |_| {},
            )
            .await;
            assert!(
                matches!(result, Err(RunError::Attempt(FailoverError { classification, .. })) if classification.kind == kind)
            );
            assert_eq!(attempts.len(), 1);
        }
    }
    #[tokio::test]
    async fn honors_the_rate_limit_block_duration() {
        for (stderr, expected_block_ms) in [
            ("HTTP 429 rate limit retry-after-ms: 2500", 2500.0),
            ("HTTP 429 rate limit", 60000.0),
            (
                "HTTP 429 rate limit retry-after: 200000",
                48.0 * 60.0 * 60.0 * 1000.0,
            ),
        ] {
            let store = store_with(["alpha", "bravo", "charlie"].map(pool_slot).into()).await;
            let mut attempts = Vec::new();
            run_failover(
                FailoverOptions {
                    store: &store,
                    provider_id: "cursor-cli-oauth",
                    affinity: CursorAffinityOptions {
                        session_id: Some("duration"),
                        ..Default::default()
                    },
                },
                |account, _| one_failure_then(&mut attempts, account, json!({"stderr":stderr})),
                || 10000.0,
                |_| {},
            )
            .await
            .unwrap_or_else(|_| panic!("rotated"));
            let credential = store
                .read("cursor-cli-oauth", None)
                .await
                .expect("read")
                .expect("credential")
                .into_oauth()
                .expect("oauth");
            let blocked = list_accounts(&credential)
                .expect("accounts")
                .into_iter()
                .find(|a| a.name == attempts[0])
                .expect("blocked");
            assert_eq!(blocked.block_reason, Some(BlockReason::RateLimit));
            assert_eq!(blocked.blocked_until, Some(10000.0 + expected_block_ms));
        }
    }
}
