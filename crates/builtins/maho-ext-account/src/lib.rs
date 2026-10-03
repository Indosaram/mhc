#[derive(Debug, PartialEq, Eq)]
pub enum AccountAction { List, Pin(String), Unpin, Remove(String), DisplayName(String), Usage }

pub fn parse_command(raw: &str) -> Option<(String, AccountAction)> {
    let raw = raw.trim();
    let mut args = raw.split_whitespace();
    let provider = args.next()?;
    let remainder = raw[provider.len()..].trim_start();
    let action = match args.next().unwrap_or("list") {
        "list" => AccountAction::List,
        "pin" => args.next().map_or(AccountAction::Usage, |id| AccountAction::Pin(id.into())),
        "unpin" => AccountAction::Unpin,
        "remove" => args.next().map_or(AccountAction::Usage, |id| AccountAction::Remove(id.into())),
        "rename" | "clear-name" => AccountAction::DisplayName(remainder.into()),
        _ => AccountAction::Usage,
    };
    Some((provider.into(), action))
}

use maho_ext_api::{CredentialAccountSource, CredentialAccountSummary, Extension, ExtensionApi, ExtensionFailure, NotificationType};
use std::sync::Arc;

fn account_label(name: &str, display_name: Option<&str>) -> String {
    display_name.map_or_else(|| name.into(), |display| format!("{display} ({name})"))
}
fn account_status(account: &CredentialAccountSummary) -> String {
    let source = match account.source { CredentialAccountSource::Login => "login", CredentialAccountSource::Import => "import", CredentialAccountSource::Env => "env" };
    let mut status = format!("{} | {} | {}", account_label(&account.name, account.display_name.as_deref()), source, if account.blocked { "blocked" } else { "available" });
    if account.pinned { status.push_str(" | pinned"); }
    status
}
pub struct Account;
impl Extension for Account {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_command("account", Some("List and manage credential accounts for any provider.".into()), Some("<provider> [list | pin <id> | unpin | remove <id> | rename <id> <display name...> | clear-name <id>]".into()), Arc::new(|raw, ctx| Box::pin(async move {
            let operation = async {
                let (provider, action) = parse_command(raw).ok_or_else(|| ExtensionFailure::new("Usage: /account <provider> [list | pin <id> | unpin | remove <id> | rename <id> <display name...> | clear-name <id>]"))?;
                let registry = &ctx.model_registry;
                let message = match action {
                    AccountAction::List => {
                        let accounts = registry.get_credential_accounts(&provider).await?;
                        let mut lines = vec![format!("Credential accounts for {provider}:")];
                        if accounts.is_empty() { lines.push("  (none)".into()); }
                        lines.extend(accounts.iter().map(|account| format!("  {}", account_status(account))));
                        lines.join("\n")
                    }
                    AccountAction::Pin(name) => { registry.pin_credential_account(&provider, Some(&name)).await?; format!("Pinned {provider} account '{name}'.") }
                    AccountAction::Unpin => { registry.pin_credential_account(&provider, None).await?; format!("Unpinned {provider} account.") }
                    AccountAction::Remove(name) => { registry.remove_credential_account(&provider, &name).await?; format!("Removed {provider} account '{name}'.") }
                    AccountAction::DisplayName(args) => {
                        maho_ext_builtin_loose::account_display_name::account_display_name_command(ctx,&provider,&args).await;
                        return Ok(());
                    }
                    AccountAction::Usage => return Err(ExtensionFailure::new("Usage: /account <provider> [list | pin <id> | unpin | remove <id> | rename <id> <display name...> | clear-name <id>]")),
                };
                ctx.ui.notify(&message, NotificationType::Info);
                Ok::<(), ExtensionFailure>(())
            }.await;
            if let Err(error) = operation { ctx.ui.notify(&error.message, NotificationType::Error); }
            Ok(())
        })));
    }
}
