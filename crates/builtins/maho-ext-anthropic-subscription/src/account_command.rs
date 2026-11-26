use crate::{
    account_list::{ClaudeAccountListInput, format_claude_account_list},
    account_management::{
        add_provider_account, pin_provider_account_in_runtime, remove_provider_account_in_runtime,
        require_provider_consent, unblock_provider_account_in_runtime,
    },
    affinity::{AffinityOptions, PinSource, select_account},
    provider_id::ANTHROPIC_SUBSCRIPTION_PROVIDER_ID as PROVIDER,
};
use maho_ext_api::{ExtensionApi, ExtensionFailure, NotificationType};
use std::{collections::BTreeMap, sync::Arc};
static SESSION_PINS: std::sync::LazyLock<std::sync::Mutex<BTreeMap<String, String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));
pub fn session_pin(session: Option<&str>) -> Option<String> {
    SESSION_PINS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session?)
        .cloned()
}
const ARGUMENT_HINT: &str = "[add | remove <id> | pin <id> | unpin | unblock <id> | rename <id> <display name...> | clear-name <id>]";

fn is_login_cancelled(error: &anyhow::Error) -> bool {
    error.is::<maho_ext_builtin_loose::oauth_login_interaction::LoginCancelled>()
        || error.is::<maho_ai::utils::abort::AbortReason>()
}

fn resolve_pin<'a>(
    cli: Option<&'a str>,
    settings: Option<&'a str>,
    stored: Option<&'a str>,
) -> (Option<&'a str>, PinSource) {
    if cli.is_some() {
        (cli, PinSource::Cli)
    } else if settings.is_some() {
        (settings, PinSource::Settings)
    } else {
        (stored, PinSource::Stored)
    }
}

fn list_message(
    credential: &maho_ai::auth::types::OAuthCredential,
    settings: &crate::settings::ProviderSettings,
    environment: &BTreeMap<String, String>,
    session: &str,
    now: f64,
) -> anyhow::Result<String> {
    let accounts =
        crate::accounts::list_accounts(credential, Some(&|key| environment.get(key).cloned()))?;
    let cli = session_pin(Some(session));
    let (pin, source) = resolve_pin(
        cli.as_deref(),
        settings
            .values
            .get("pinnedAccount")
            .and_then(serde_json::Value::as_str),
        credential.get_extra_str("pinned"),
    );
    let order = credential
        .get_extra("order")
        .and_then(|order| serde_json::from_value::<Vec<String>>(order.clone()).ok());
    let eligible = accounts
        .iter()
        .filter(|account| {
            account.sign_in_required.is_none()
                && account.block_reason.as_deref() != Some("account_disabled")
        })
        .cloned()
        .collect::<Vec<_>>();
    let pick = (!accounts.is_empty()).then(|| {
        select_account(
            &eligible,
            &AffinityOptions {
                session_id: Some(session),
                pinned_account: pin,
                pin_source: pin.map(|_| source),
                account_fallback: settings
                    .values
                    .get("accountFallback")
                    .and_then(serde_json::Value::as_bool),
                account_order: order.as_deref(),
                ..Default::default()
            },
            now,
        )
    });
    let error = pick
        .as_ref()
        .and_then(|result| result.as_ref().err())
        .map(ToString::to_string);
    Ok(format_claude_account_list(ClaudeAccountListInput {
        accounts: &accounts,
        pinned: pin,
        pin_source: source,
        affinity_pick: pick
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|slot| slot.name.as_str()),
        affinity_error: error.as_deref(),
        now,
    }))
}

/// senpi `accountStatus`: an account-wide block, an auth block saying when it is re-checked.
fn account_status(slot: &crate::accounts::AccountSlot, now: f64) -> Option<String> {
    let until = slot.blocked_until.filter(|until| *until > now)?;
    let until = crate::model_blocks::iso_ms(until);
    Some(if slot.block_reason.as_deref() == Some("auth_error") {
        format!("login rejected; re-checked at {until}")
    } else {
        format!("blocked until {until}")
    })
}

/// senpi `slotStatus`: the account-wide block followed by each live model-family block (senpi#2555).
pub fn slot_status(slot: &crate::accounts::AccountSlot, now: f64) -> String {
    let states: Vec<String> = account_status(slot, now)
        .into_iter()
        .chain(crate::model_blocks::describe_model_blocks(
            slot.model_blocks.as_ref(),
            now,
        ))
        .collect();
    if states.is_empty() {
        "available".into()
    } else {
        states.join(", ")
    }
}

pub fn register(
    api: &mut ExtensionApi,
    oauth: Arc<crate::oauth_login::AnthropicSubscriptionOAuth>,
    environment: Arc<BTreeMap<String, String>>,
) {
    let store = oauth.store.clone();
    api.register_flag(
        "claude-account",
        maho_ext_api::FlagType::String { default: None },
        Some("Pin Anthropic Subscription account for this session.".into()),
    );
    for kind in [
        maho_ext_api::EventKind::SessionStart,
        maho_ext_api::EventKind::SessionShutdown,
    ] {
        let runtime = api.runtime.clone();
        api.on(
            kind,
            Arc::new(move |event, ctx| {
                let runtime = runtime.clone();
                Box::pin(async move {
                    let mut pins = SESSION_PINS
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let session = ctx.session_manager.session_id();
                    crate::guidance::PRESET_APPEND_DEPRECATION
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .reset(Some(session));
                    pins.remove(session);
                    if event.kind() == maho_ext_api::EventKind::SessionStart
                        && let Some(maho_ext_api::FlagValue::String(pin)) =
                            runtime.get_flag("claude-account")
                        && !pin.is_empty()
                    {
                        pins.insert(session.into(), pin);
                    }
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );
    }
    let runtime = api.runtime.clone();
    api.register_command("claude-account", Some("List and manage Anthropic Subscription accounts.".into()), Some(ARGUMENT_HINT.into()), Arc::new(move |raw, ctx| {
        let store = store.clone(); let environment = environment.clone(); let oauth = oauth.clone();let runtime=runtime.clone();
        Box::pin(async move {
            runtime.assert_active()?;
            let args: Vec<_> = raw.split_whitespace().collect();
            let outcome: anyhow::Result<()> = async {
                if let Some(command) = maho_ext_builtin_loose::account_display_name::parse_display_name_command(raw).map_err(anyhow::Error::msg)? {
                    let mutation_runtime=runtime.clone();
                    store.modify(PROVIDER, Box::new(move |current| Box::pin(async move {
                        mutation_runtime.assert_active()?;
                        let current = current.ok_or_else(|| anyhow::anyhow!("Stored provider account not found: {}", command.account_id))?;
                        let pooled = maho_ai::auth::pool::slots::PooledCredential::from(current.clone());
                        let next = maho_ai::auth::pool::slots::rename_slot_display_name(&pooled, &command.account_id, command.display_name.as_deref()).map_err(anyhow::Error::msg)?;
                        let mut current = current.into_oauth().ok_or_else(|| anyhow::anyhow!("Provider account management requires an OAuth credential"))?;
                        let display = next.accounts.into_iter().flatten().find(|slot| slot.name == command.account_id).ok_or_else(|| anyhow::anyhow!("Stored provider account not found: {}", command.account_id))?.display_name;
                        for slot in current.extra.get_mut("accounts").and_then(serde_json::Value::as_array_mut).ok_or_else(|| anyhow::anyhow!("Stored provider account not found: {}", command.account_id))? {
                            if slot["name"] == command.account_id && let Some(object) = slot.as_object_mut() {
                                if let Some(display) = &display { object.insert("displayName".into(), serde_json::json!(display)); } else { object.remove("displayName"); }
                            }
                        }
                        Ok(Some(maho_ai::auth::types::Credential::OAuth(current)))
                    })), None).await?;
                    runtime.assert_active()?;
                    crate::account_events::emit_provider_accounts_changed(PROVIDER);
                    return Ok(());
                }
                match args.as_slice() {
                    ["add"] => {
                        if !ctx.has_ui { anyhow::bail!("/claude-account add requires an interactive UI."); }
                        require_provider_consent(&ctx.agent_dir)?;
                        use maho_ai::auth::types::{AuthInteraction, ProviderAuthInteraction};
                        let interaction: Arc<dyn AuthInteraction> = Arc::new(maho_ext_builtin_loose::oauth_login_interaction::ExtensionLoginInteraction::new(ctx.ui.clone(), ctx.mode, crate::oauth_login::PROVIDER_NAME.into(), Some(PROVIDER), Arc::new(|url| {
                            let url=url.to_owned();
                            tokio::spawn(async move {
                                let (program,args)=match std::env::consts::OS {"macos"=>("open",vec![url]),"windows"=>("rundll32",vec!["url.dll,FileProtocolHandler".into(),url]),_=>("xdg-open",vec![url])};
                                let result=tokio::process::Command::new(program).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().await;
                                if !result.is_ok_and(|status|status.success()) {maho_ext_api::diagnostic("OAuth browser launcher failed; use the displayed authorization URL.");}
                            });
                        })));
                        let signal = interaction.signal().ok_or_else(|| anyhow::anyhow!("Login cancellation is not available"))?;
                        let interaction = ProviderAuthInteraction::new(signal, interaction);
                        runtime.assert_active()?;
                        match add_provider_account(oauth.as_ref(), &interaction, &ctx.agent_dir).await {
                            Ok(_) => {}
                            Err(error) if is_login_cancelled(&error) => return Ok(()),
                            Err(error) => return Err(error.context("Failed to add Anthropic Subscription account")),
                        }
                        runtime.assert_active()?;
                        ctx.ui.notify("Anthropic Subscription account added.", NotificationType::Info);
                    },
                    [] | ["list"] => {
                        let now = maho_ai::utils::diagnostics::now_ms() as f64;
                        let stored = store.read(PROVIDER, None).await?;
                        let empty = crate::accounts::empty_credential();
                        let credential = stored.as_ref().and_then(|value| value.as_oauth()).unwrap_or(&empty);
                        let message = list_message(credential, &(oauth.settings)(), &environment, ctx.session_manager.session_id(), now)?;
                        runtime.assert_active()?;
                        ctx.ui.notify(&message, NotificationType::Info);
                    },
                    ["pin", "unpin"] | ["unpin"] => {
                        let stored = store.read(PROVIDER, None).await?;
                        if stored.as_ref().and_then(|value| value.as_oauth()).and_then(|credential| credential.get_extra_str("pinned")).is_none() {
                            runtime.assert_active()?;
                            ctx.ui.notify("No stored Anthropic Subscription account pin is set.", NotificationType::Info);
                        } else {
                            pin_provider_account_in_runtime(store.as_ref(), PROVIDER, None, environment, Some(runtime.clone())).await?;
                            ctx.ui.notify("Unpinned Anthropic Subscription account.", NotificationType::Info);
                        }
                    },
                    ["pin", name] => {
                        pin_provider_account_in_runtime(store.as_ref(), PROVIDER, Some(name), environment, Some(runtime.clone())).await?;
                        ctx.ui.notify(&format!("Pinned Anthropic Subscription account: {name}."), NotificationType::Info);
                    },
                    ["remove", name] => {
                            let controller = maho_ai::utils::abort::AbortController::new();
                            let removal = remove_provider_account_in_runtime(
                                oauth.as_ref(), name, environment, Some(runtime.clone()),
                                Some(controller.signal()),
                            );
                            tokio::pin!(removal);
                            let result = if let Some(signal) = &ctx.signal {
                                tokio::select! {
                                    biased;
                                    () = signal.cancelled() => {
                                        controller.abort(None);
                                        removal.await
                                    }
                                    result = &mut removal => result,
                                }
                            } else {
                                removal.await
                            }?;
                        let (message, kind) = result.notice.map_or_else(
                            || (format!("Removed Anthropic Subscription account: {name}."), NotificationType::Info),
                            |notice| (notice, NotificationType::Warning),
                        );
                        ctx.ui.notify(&message, kind);
                    },
                    ["unblock", name] => { let lifted=unblock_provider_account_in_runtime(store.as_ref(),PROVIDER,name,environment,Some(runtime.clone())).await?;
                        runtime.assert_active()?;ctx.ui.notify(&if lifted {format!("Unblocked Anthropic Subscription account: {name}. The next turn may use it.")}else {format!("Anthropic Subscription account '{name}' has no usage block to lift.")},NotificationType::Info); }
                    [action @ ("remove" | "pin" | "unblock")] => ctx.ui.notify(&format!("Usage: /claude-account {action} <name>"), NotificationType::Error),
                    _ => ctx.ui.notify(&format!("Usage: /claude-account {ARGUMENT_HINT}"), NotificationType::Error),
                }
                Ok(())
            }.await;
            if let Err(error) = outcome {runtime.assert_active()?; ctx.ui.notify(&format!("{error:#}"), NotificationType::Error); }
            Ok::<(), ExtensionFailure>(())
        })
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_pin_missing_from_the_pool_is_reported_even_when_empty() {
        let mut settings = crate::settings::ProviderSettings::default();
        settings
            .values
            .insert("pinnedAccount".into(), "gone".into());
        settings
            .values
            .insert("accountFallback".into(), false.into());
        let empty = crate::accounts::empty_credential();
        let output = list_message(
            &empty,
            &settings,
            &BTreeMap::new(),
            "missing-pin-fixture",
            0.0,
        )
        .expect("empty list");
        assert_eq!(
            output.lines().collect::<Vec<_>>(),
            [
                "Anthropic Subscription accounts: none",
                "Pinned account: gone (settings) - not in this list",
                "Add one with /claude-account add.",
            ]
        );
        let credential = crate::accounts::add_account(
            &empty,
            crate::accounts::AccountSlot {
                name: "alpha".into(),
                ..Default::default()
            },
        )
        .expect("slot");
        let output = list_message(
            &credential,
            &settings,
            &BTreeMap::new(),
            "missing-pin-fixture",
            0.0,
        )
        .expect("nonempty list");
        assert!(output.contains("Pinned account: gone (settings) - not in this list"));
        assert!(
            output
                .lines()
                .any(|line| line.starts_with("Next turn: unavailable - "))
        );
        assert!(!output.contains("[pinned") && !output.contains("[next turn"));
    }

    #[test]
    fn cli_pin_overrides_settings_and_stored_pin_in_next_turn_preview() {
        let mut settings = crate::settings::ProviderSettings::default();
        settings
            .values
            .insert("pinnedAccount".into(), "settings".into());
        let empty = crate::accounts::empty_credential();
        let credential =
            ["cli", "settings", "stored"]
                .into_iter()
                .fold(empty, |credential, name| {
                    crate::accounts::add_account(
                        &credential,
                        crate::accounts::AccountSlot {
                            name: name.into(),
                            ..Default::default()
                        },
                    )
                    .expect("slot")
                });
        let credential = crate::accounts::pin_account(&credential, "stored");
        let session = "cli-pin-list-fixture";
        SESSION_PINS
            .lock()
            .expect("pins")
            .insert(session.into(), "cli".into());
        let output = list_message(&credential, &settings, &BTreeMap::new(), session, 0.0);
        SESSION_PINS.lock().expect("pins").remove(session);
        let output = output.expect("list");
        assert!(
            output
                .lines()
                .any(|line| line == "cli  [pinned for this session, next turn]")
        );
        assert!(output.lines().any(|line| line == "settings"));
        assert!(output.lines().any(|line| line == "stored"));
    }

    #[tokio::test]
    async fn stored_pin_overrides_affinity_and_unknown_pin_preserves_it() {
        use maho_ai::auth::{
            credential_store::InMemoryCredentialStore,
            types::{Credential, CredentialStore},
        };
        let store = InMemoryCredentialStore::new();
        let empty = crate::accounts::empty_credential();
        let credential =
            ["alpha", "bravo", "charlie"]
                .into_iter()
                .fold(empty, |credential, name| {
                    crate::accounts::add_account(
                        &credential,
                        crate::accounts::AccountSlot {
                            name: name.into(),
                            ..Default::default()
                        },
                    )
                    .expect("slot")
                });
        store
            .modify(
                PROVIDER,
                Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })),
                None,
            )
            .await
            .expect("seed");
        crate::account_management::pin_provider_account(
            &store,
            PROVIDER,
            Some("bravo"),
            Arc::default(),
        )
        .await
        .expect("pin");
        let error = crate::account_management::pin_provider_account(
            &store,
            PROVIDER,
            Some("unknown"),
            Arc::default(),
        )
        .await
        .expect_err("unknown pin");
        assert_eq!(error.to_string(), "Provider account not found: unknown");
        let saved = store
            .read(PROVIDER, None)
            .await
            .expect("read")
            .and_then(Credential::into_oauth)
            .expect("credential");
        let output = list_message(
            &saved,
            &Default::default(),
            &BTreeMap::new(),
            "session-01",
            0.0,
        )
        .expect("list");
        assert!(
            output
                .lines()
                .any(|line| line == "bravo  [pinned, next turn]")
        );
    }

    #[test]
    fn slot_status_lists_the_account_block_then_each_model_block() {
        let mut slot = crate::accounts::AccountSlot {
            name: "work".into(),
            ..Default::default()
        };
        assert_eq!(slot_status(&slot, 0.0), "available");
        slot.model_blocks = Some(
            [(
                "opus".to_owned(),
                crate::model_blocks::ModelBlock {
                    blocked_until: 60_000.0,
                },
            )]
            .into(),
        );
        assert_eq!(
            slot_status(&slot, 0.0),
            "blocked for opus until 1970-01-01T00:01:00.000Z"
        );
        slot.blocked_until = Some(1_000.0);
        slot.block_reason = Some("auth_error".into());
        assert_eq!(
            slot_status(&slot, 0.0),
            "login rejected; re-checked at 1970-01-01T00:00:01.000Z, blocked for opus until 1970-01-01T00:01:00.000Z"
        );
        slot.block_reason = Some("rate_limit".into());
        assert_eq!(
            slot_status(&slot, 2_000.0),
            "blocked for opus until 1970-01-01T00:01:00.000Z"
        );
    }
}
#[cfg(test)]
mod cancellation_tests {
    use super::is_login_cancelled;
    use maho_ext_builtin_loose::oauth_login_interaction::{
        LOGIN_CANCELLED_MESSAGE, LoginCancelled,
    };

    #[test]
    fn cancellation_is_classified_by_type_not_matching_error_text() {
        assert!(is_login_cancelled(&anyhow::Error::new(LoginCancelled)));
        assert!(!is_login_cancelled(&anyhow::anyhow!(
            LOGIN_CANCELLED_MESSAGE
        )));
        assert!(is_login_cancelled(&anyhow::Error::new(
            maho_ai::utils::abort::AbortReason::dom_default(),
        )));
    }
}
