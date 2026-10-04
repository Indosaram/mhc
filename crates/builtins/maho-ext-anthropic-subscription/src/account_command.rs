use std::{collections::BTreeMap, sync::Arc};
use maho_ext_api::{ExtensionApi, NotificationType, ExtensionFailure};
use crate::account_management::{get_provider_accounts, pin_provider_account_in_runtime, remove_provider_account_in_runtime, ANTHROPIC_SUBSCRIPTION_PROVIDER_ID as PROVIDER};
static SESSION_PINS: std::sync::LazyLock<std::sync::Mutex<BTreeMap<String, String>>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));
pub fn session_pin(session: Option<&str>) -> Option<String> { SESSION_PINS.lock().expect("session pins").get(session?).cloned() }

pub fn register(api: &mut ExtensionApi, oauth: Arc<crate::oauth_login::AnthropicSubscriptionOAuth>, environment: Arc<BTreeMap<String, String>>) {
    let store = oauth.store.clone();
    api.register_flag("claude-account", maho_ext_api::FlagType::String { default: None }, Some("Pin Anthropic Subscription account for this session.".into()));
    for kind in [maho_ext_api::EventKind::SessionStart, maho_ext_api::EventKind::SessionShutdown] {
        let runtime = api.runtime.clone();
        api.on(kind, Arc::new(move |event, ctx| {
            let runtime = runtime.clone(); Box::pin(async move {
                let mut pins = SESSION_PINS.lock().expect("session pins");
                let session = ctx.session_manager.session_id();
                crate::guidance::PRESET_APPEND_DEPRECATION.lock().expect("prompt guidance").reset(Some(session));
                pins.remove(session);
                if event.kind() == maho_ext_api::EventKind::SessionStart
                    && let Some(maho_ext_api::FlagValue::String(pin)) = runtime.get_flag("claude-account")
                    && !pin.is_empty() { pins.insert(session.into(), pin); }
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
    let runtime=api.runtime.clone();
    api.register_command("claude-account", Some("List and manage Anthropic Subscription accounts.".into()), Some("[remove <id> | pin <id> | unpin]".into()), Arc::new(move |raw, ctx| {
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
                        let display = next.accounts.expect("renamed accounts").into_iter().find(|slot| slot.name == command.account_id).expect("renamed slot").display_name;
                        for slot in current.extra.get_mut("accounts").and_then(serde_json::Value::as_array_mut).ok_or_else(|| anyhow::anyhow!("Stored provider account not found: {}", command.account_id))? {
                            if slot["name"] == command.account_id {
                                let object = slot.as_object_mut().expect("slot");
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
                        use maho_ai::auth::types::{AuthInteraction, OAuthAuth, Credential, ProviderAuthInteraction};
                        let interaction = Arc::new(maho_ext_builtin_loose::oauth_login_interaction::ExtensionLoginInteraction::new(ctx.ui.clone(), ctx.mode, crate::oauth_login::PROVIDER_NAME.into(), Some(PROVIDER), Arc::new(|url| {
                            let url=url.to_owned();
                            tokio::spawn(async move {
                                let (program,args)=match std::env::consts::OS {"macos"=>("open",vec![url]),"windows"=>("rundll32",vec!["url.dll,FileProtocolHandler".into(),url]),_=>("xdg-open",vec![url])};
                                let result=tokio::process::Command::new(program).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().await;
                                if !result.is_ok_and(|status|status.success()) {eprintln!("OAuth browser launcher failed; use the displayed authorization URL.");}
                            });
                        })));
                        let interaction = ProviderAuthInteraction::new(interaction.signal().expect("login signal"), interaction);
                        let credential = oauth.login(&interaction).await?;
                        runtime.assert_active()?;
                        let mutation_runtime=runtime.clone();
                        store.modify(PROVIDER, Box::new(move |_| Box::pin(async move {mutation_runtime.assert_active()?; Ok(Some(Credential::OAuth(credential))) })), None).await?;
                        runtime.assert_active()?;
                        crate::account_events::emit_provider_accounts_changed(PROVIDER);
                    },
                    [] | ["list"] => {
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as f64;
                        let accounts = get_provider_accounts(store.as_ref(), PROVIDER, &environment, now).await?;
                        let message = if accounts.is_empty() { "No Anthropic Subscription accounts configured.".into() } else { accounts.into_iter().map(|account| format!("{}{}{}", account.name, if account.pinned { " (pinned)" } else { "" }, if account.blocked { " (blocked)" } else { "" })).collect::<Vec<_>>().join("\n") };
                        runtime.assert_active()?;
                        ctx.ui.notify(&message, NotificationType::Info);
                    },
                    ["pin", "unpin"] | ["unpin"] => pin_provider_account_in_runtime(store.as_ref(), PROVIDER, None, environment, Some(runtime.clone())).await?,
                    ["pin", name] => pin_provider_account_in_runtime(store.as_ref(), PROVIDER, Some(name), environment, Some(runtime.clone())).await?,
                    ["remove", name] => remove_provider_account_in_runtime(store.as_ref(), PROVIDER, name, environment, Some(runtime.clone())).await?,
                    _ => ctx.ui.notify("Usage: /claude-account [remove <id> | pin <id> | unpin]", NotificationType::Warning),
                }
                Ok(())
            }.await;
            if let Err(error) = outcome {runtime.assert_active()?; ctx.ui.notify(&error.to_string(), NotificationType::Error); }
            Ok::<(), ExtensionFailure>(())
        })
    }));
}
