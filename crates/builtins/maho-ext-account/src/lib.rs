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
