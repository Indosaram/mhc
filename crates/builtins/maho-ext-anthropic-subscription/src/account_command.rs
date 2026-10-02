use std::{collections::BTreeMap, sync::Arc};
use maho_ext_api::{ExtensionApi, NotificationType, ExtensionFailure};
use crate::account_management::{get_provider_accounts, pin_provider_account, remove_provider_account, ANTHROPIC_SUBSCRIPTION_PROVIDER_ID as PROVIDER};

pub fn register(api: &mut ExtensionApi, oauth: Arc<crate::oauth_login::AnthropicSubscriptionOAuth>, environment: Arc<BTreeMap<String, String>>) {
    let store = oauth.store.clone();
    api.register_command("claude-account", Some("List and manage Anthropic Subscription accounts.".into()), Some("[remove <id> | pin <id> | unpin]".into()), Arc::new(move |raw, ctx| {
        let store = store.clone(); let environment = environment.clone(); let oauth = oauth.clone();
        Box::pin(async move {
            let args: Vec<_> = raw.split_whitespace().collect();
            let outcome: anyhow::Result<()> = async {
                if let Some(command) = maho_ext_builtin_loose::account_display_name::parse_display_name_command(raw).map_err(anyhow::Error::msg)? {
                    store.modify(PROVIDER, Box::new(move |current| Box::pin(async move {
                        let current = current.ok_or_else(|| anyhow::anyhow!("Stored provider account not found: {}", command.account_id))?;
                        let pooled = maho_ai::auth::pool::slots::PooledCredential::from(current);
                        let next = maho_ai::auth::pool::slots::rename_slot_display_name(&pooled, &command.account_id, command.display_name.as_deref()).map_err(anyhow::Error::msg)?;
                        Ok(Some(next.to_stored_credential()))
                    })), None).await?;
                    crate::account_events::emit_provider_accounts_changed(PROVIDER);
                    return Ok(());
                }
                match args.as_slice() {
                    ["add"] => {
                        use maho_ai::auth::types::{AuthInteraction, OAuthAuth, Credential, ProviderAuthInteraction};
                        let interaction = Arc::new(maho_ext_builtin_loose::oauth_login_interaction::ExtensionLoginInteraction::new(ctx.ui.clone(), ctx.mode, crate::oauth_login::PROVIDER_NAME.into(), Some(PROVIDER), Arc::new(|_| {})));
                        let interaction = ProviderAuthInteraction::new(interaction.signal().expect("login signal"), interaction);
                        let credential = oauth.login(&interaction).await?;
                        store.modify(PROVIDER, Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })), None).await?;
                        crate::account_events::emit_provider_accounts_changed(PROVIDER);
                    },
                    [] | ["list"] => {
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as f64;
                        let accounts = get_provider_accounts(store.as_ref(), PROVIDER, &environment, now).await?;
                        let message = if accounts.is_empty() { "No Anthropic Subscription accounts configured.".into() } else { accounts.into_iter().map(|account| format!("{}{}{}", account.name, if account.pinned { " (pinned)" } else { "" }, if account.blocked { " (blocked)" } else { "" })).collect::<Vec<_>>().join("\n") };
                        ctx.ui.notify(&message, NotificationType::Info);
                    },
                    ["pin", "unpin"] | ["unpin"] => pin_provider_account(store.as_ref(), PROVIDER, None, environment).await?,
                    ["pin", name] => pin_provider_account(store.as_ref(), PROVIDER, Some(name), environment).await?,
                    ["remove", name] => remove_provider_account(store.as_ref(), PROVIDER, name, environment).await?,
                    _ => ctx.ui.notify("Usage: /claude-account [remove <id> | pin <id> | unpin]", NotificationType::Warning),
                }
                Ok(())
            }.await;
            if let Err(error) = outcome { ctx.ui.notify(&error.to_string(), NotificationType::Error); }
            Ok::<(), ExtensionFailure>(())
        })
    }));
}
