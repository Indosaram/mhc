#[derive(Debug, PartialEq, Eq)]
pub enum AccountAction {
    List,
    Pin(String),
    Unpin,
    Reset(String),
    Remove(String),
    DisplayName(String),
    Usage,
}

pub fn parse_command(raw: &str) -> Option<(String, AccountAction)> {
    let raw = raw.trim();
    let mut args = raw.split_whitespace();
    let provider = args.next()?;
    let remainder = raw[provider.len()..].trim_start();
    let action = match args.next().unwrap_or("list") {
        "list" => AccountAction::List,
        "pin" => args
            .next()
            .map_or(AccountAction::Usage, |id| AccountAction::Pin(id.into())),
        "unpin" => AccountAction::Unpin,
        "reset" => args
            .next()
            .map_or(AccountAction::Usage, |id| AccountAction::Reset(id.into())),
        "remove" => args
            .next()
            .map_or(AccountAction::Usage, |id| AccountAction::Remove(id.into())),
        "rename" | "clear-name" => AccountAction::DisplayName(remainder.into()),
        _ => AccountAction::Usage,
    };
    Some((provider.into(), action))
}

use maho_ext_api::{
    CredentialAccountSource, CredentialAccountSummary, Extension, ExtensionApi, ExtensionFailure,
    NotificationType,
};
use std::sync::Arc;

fn account_label(name: &str, display_name: Option<&str>) -> String {
    display_name.map_or_else(|| name.into(), |display| format!("{display} ({name})"))
}
pub fn account_status(account: &CredentialAccountSummary) -> String {
    let source = match account.source {
        CredentialAccountSource::Login => "login",
        CredentialAccountSource::Import => "import",
        CredentialAccountSource::Env => "env",
    };
    let availability = if account.unused {
        "unused (other login kind; switch with /login)"
    } else if account.blocked {
        "blocked"
    } else {
        "available"
    };
    let mut status = format!(
        "{} | {} | {}",
        account_label(&account.name, account.display_name.as_deref()),
        source,
        availability
    );
    for block in &account.blocked_models {
        status.push_str(&format!(
            " | blocked for {} until {}",
            block.model,
            iso_timestamp(block.until)
        ));
    }
    if account.pinned {
        status.push_str(" | pinned");
    }
    status
}

/// JS `new Date(ms).toISOString()` (`YYYY-MM-DDTHH:MM:SS.sssZ`, UTC) without a date crate.
pub fn iso_timestamp(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let day_ms = ms.rem_euclid(86_400_000);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        day_ms / 3_600_000,
        day_ms / 60_000 % 60,
        day_ms / 1_000 % 60,
        day_ms % 1_000
    )
}
pub struct Account;
impl Extension for Account {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_command("account", Some("List and manage credential accounts for any provider.".into()), Some("<provider> [list | pin <id> | unpin | reset <id> | remove <id> | rename <id> <display name...> | clear-name <id>]".into()), Arc::new(|raw, ctx| Box::pin(async move {
            let operation = async {
                let usage = "Usage: /account <provider> [list | pin <id> | unpin | reset <id> | remove <id> | rename <id> <display name...> | clear-name <id>]";
                let (provider, action) = parse_command(raw).ok_or_else(|| ExtensionFailure::new(usage))?;
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
                    AccountAction::Reset(name) => { registry.reset_credential_account(&provider, &name).await?; format!("Cleared the cooldown on {provider} account '{name}'; the next request tries it again.") }
                    AccountAction::Remove(name) => { registry.remove_credential_account(&provider, &name).await?; format!("Removed {provider} account '{name}'.") }
                    AccountAction::DisplayName(args) => {
                        maho_ext_builtin_loose::account_display_name::account_display_name_command(ctx,&provider,&args).await;
                        return Ok(());
                    }
                    AccountAction::Usage => return Err(ExtensionFailure::new(usage)),
                };
                ctx.ui.notify(&message, NotificationType::Info);
                Ok::<(), ExtensionFailure>(())
            }.await;
            if let Err(error) = operation { ctx.ui.notify(&error.message, NotificationType::Error); }
            Ok(())
        })));
    }
}
