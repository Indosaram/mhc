use crate::account_display_name::{DisplayNameCommand, parse_display_name_command};

pub const PROVIDER_ID: &str = "chatgpt-subscription";
pub const PROVIDER_LABEL: &str = "ChatGPT Subscription OAuth";

#[derive(Debug, PartialEq, Eq)]
pub enum AccountAction {
    DisplayName(DisplayNameCommand),
    List,
    Add,
    Remove(String),
    Pin(String),
    Unpin,
    Usage,
}

pub fn parse_action(raw: &str) -> Result<AccountAction, &'static str> {
    if let Some(command) = parse_display_name_command(raw)? { return Ok(AccountAction::DisplayName(command)); }
    let mut args = raw.split_whitespace();
    Ok(match args.next().unwrap_or("list") {
        "list" => AccountAction::List,
        "add" => AccountAction::Add,
        "remove" => args.next().map_or(AccountAction::Usage, |id| AccountAction::Remove(id.into())),
        "pin" => match args.next() {
            Some("unpin") => AccountAction::Unpin,
            Some(id) => AccountAction::Pin(id.into()),
            None => AccountAction::Usage,
        },
        "unpin" => AccountAction::Unpin,
        _ => AccountAction::Usage,
    })
}
